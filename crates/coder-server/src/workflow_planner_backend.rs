use std::{collections::BTreeSet, time::Duration};

use async_trait::async_trait;
use coder_core::FinalReport;
use coder_harness::{
    HarnessBackend, HarnessError, HarnessRunEvent, HarnessRunRequest, HarnessRunResult,
};
use coder_workflow::PlannerModelBackend;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api_types::PlannerProviderTrace;
use crate::model_tool_async_attachments::pending_planner_user_guidance_count;
use crate::planner_provider_recovery::read_planner_provider_error_body;
use crate::planner_provider_runtime::{
    parse_live_planner_response_with_idle_timeout, planner_chat_completion_body,
    planner_provider_trace,
};
use crate::provider_runtime::{
    harness_model_spec, model_name_for_settings, model_provider_base_url,
    model_provider_for_settings, provider_api_key, provider_chat_completions_endpoint,
    provider_http_client_builder, provider_proxy_url_for_url, redact_provider_error,
};
use crate::run_token_budget::{
    check_run_token_budget, record_run_token_usage, RunTokenBudgetSnapshot, RunTokenUsage,
};
use crate::ApiState;

const WORKFLOW_PLANNER_MAX_OUTPUT_TOKENS: u32 = 900;
const WORKFLOW_PLANNER_MAX_IMPROVEMENTS: usize = 3;
const INTERRUPTED_EXECUTOR_MARKER: &str =
    "native_model_tool_loop: stopped_after_turn_limit_with_file_writes";
const COMPLETE_INTERRUPTED_EXECUTOR: &str = "Complete the interrupted implementation, inspect the current changes against each acceptance criterion, and record task-specific evidence before calling finish.";

#[derive(Clone)]
pub(crate) struct WorkflowPlannerBackend {
    state: ApiState,
    fallback: PlannerModelBackend,
}

impl WorkflowPlannerBackend {
    pub(crate) fn new(state: ApiState) -> Self {
        Self {
            state,
            fallback: PlannerModelBackend,
        }
    }

    async fn run_live(&self, request: &HarnessRunRequest) -> Result<HarnessRunResult, String> {
        let settings = self.state.provider_settings.lock().unwrap().clone();
        let model = harness_model_spec(request);
        let provider = model_provider_for_settings(&settings, &model);
        let model_name = model_name_for_settings(&settings, &model);
        let (api_key, _) = provider_api_key(&settings, &provider, model.api_key_env.as_deref())
            .ok_or_else(|| format!("workflow Planner API key is not configured for {provider}"))?;
        let base_url = model_provider_base_url(&settings, &provider, &model)
            .ok_or_else(|| format!("workflow Planner base URL is not configured for {provider}"))?;
        let url = provider_chat_completions_endpoint(&base_url);
        let proxy_url = provider_proxy_url_for_url(&settings, &provider, Some(&url));
        let redaction_values = [
            api_key.as_str(),
            base_url.as_str(),
            proxy_url.as_deref().unwrap_or(""),
        ];
        let stream_idle_timeout = workflow_planner_stream_idle_timeout(request);
        let client = provider_http_client_builder(&url, proxy_url.as_deref())
            .map_err(|error| redact_provider_error(&error, &redaction_values))?
            .timeout(stream_idle_timeout)
            .build()
            .map_err(|error| redact_provider_error(&error.to_string(), &redaction_values))?;
        let max_output_tokens = request
            .backend_context
            .pointer("/coder/agent/runtime/max_output_tokens")
            .and_then(Value::as_u64)
            .map(|value| value.clamp(256, u64::from(WORKFLOW_PLANNER_MAX_OUTPUT_TOKENS)) as u32)
            .unwrap_or(WORKFLOW_PLANNER_MAX_OUTPUT_TOKENS);
        let effort = request
            .backend_context
            .pointer("/coder/agent/runtime/effort")
            .and_then(Value::as_str);
        let body = planner_chat_completion_body(
            &provider,
            &model_name,
            workflow_planner_messages(request),
            max_output_tokens,
            effort,
        );
        let estimated_input_tokens = u64::from(crate::estimate_text_tokens(&body.to_string()));
        let response = client
            .post(&url)
            .bearer_auth(&api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                redact_provider_error(
                    &format!("workflow Planner request failed: {error}"),
                    &redaction_values,
                )
            })?;
        if !response.status().is_success() {
            let status = response.status();
            let error = read_planner_provider_error_body(response, &redaction_values).await?;
            return Err(format!(
                "workflow Planner returned HTTP {status}: {}",
                compact_text(&error.redacted, 480)
            ));
        }
        let mut trace = planner_provider_trace(false, "unknown", false, None);
        trace.estimated_input_tokens = estimated_input_tokens;
        let message = parse_live_planner_response_with_idle_timeout(
            response,
            &redaction_values,
            trace,
            stream_idle_timeout,
        )
        .await?
        .ok_or_else(|| "workflow Planner returned no decision".to_owned())?;
        let model_decision = parse_model_decision(&message.content)
            .map_err(|error| format!("workflow Planner decision was invalid: {error}"))?;
        let budget = record_run_token_usage(
            &self.state,
            request,
            RunTokenUsage {
                input_tokens: message.provider_trace.input_tokens,
                output_tokens: message.provider_trace.output_tokens,
                cache_read_tokens: message.provider_trace.cache_read_tokens,
                estimated_input_tokens: message.provider_trace.estimated_input_tokens,
                estimated_output_tokens: message.provider_trace.estimated_output_tokens,
            },
        );
        let mut decision = enforce_bounded_decision(request, model_decision);
        if budget.is_some_and(RunTokenBudgetSnapshot::exhausted) && decision.decision == "continue"
        {
            let verified_completion = completion_is_claimable(request);
            decision = apply_stop_gate(
                decision,
                verified_completion,
                "the workflow token budget was exhausted",
            );
        }
        Ok(workflow_planner_result(
            request,
            decision,
            Some(message.provider_trace),
            budget,
        ))
    }
}

#[async_trait]
impl HarnessBackend for WorkflowPlannerBackend {
    async fn run(&self, request: HarnessRunRequest) -> Result<HarnessRunResult, HarnessError> {
        if request
            .backend_context
            .pointer("/coder/agent/output_contract")
            .and_then(Value::as_str)
            != Some("workflow_decision")
            || request
                .backend_context
                .pointer("/coder/plan_context/workflow_feedback")
                .is_none()
        {
            return self.fallback.run(request).await;
        }
        if let Some(decision) = pending_guidance_decision(&self.state, &request) {
            let budget = check_run_token_budget(&self.state, &request);
            let decision = if budget.is_some_and(RunTokenBudgetSnapshot::exhausted) {
                budget_exhausted_decision()
            } else {
                decision
            };
            return Ok(workflow_planner_result(&request, decision, None, budget));
        }
        if workflow_context_str(&request, "/coder/plan_context/workflow_feedback/signal")
            == "completed"
            && !qualitative_review_requested(&request)
        {
            return self.fallback.run(request).await;
        }
        if self.state.provider_settings.lock().unwrap().mock_mode {
            return self.fallback.run(request).await;
        }
        let budget = check_run_token_budget(&self.state, &request);
        if budget.is_some_and(RunTokenBudgetSnapshot::exhausted) {
            return Ok(workflow_planner_result(
                &request,
                budget_exhausted_decision(),
                None,
                budget,
            ));
        }
        match self.run_live(&request).await {
            Ok(result) => Ok(result),
            Err(error) => Ok(workflow_planner_result(
                &request,
                bounded_stop_decision(format!("provider decision unavailable: {error}")),
                None,
                budget,
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ModelWorkflowDecision {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    improvements: Vec<String>,
    #[serde(default)]
    expected_gain: String,
    #[serde(default)]
    blockers: Vec<String>,
}

#[derive(Debug, Clone)]
struct BoundedWorkflowDecision {
    decision: String,
    summary: String,
    improvements: Vec<String>,
    expected_gain: String,
    blockers: Vec<String>,
    stop_reason: Option<String>,
}

fn pending_guidance_decision(
    state: &ApiState,
    request: &HarnessRunRequest,
) -> Option<BoundedWorkflowDecision> {
    let pending_count = pending_planner_user_guidance_count(state, &request.run_id);
    if pending_count == 0 {
        return None;
    }
    let round = workflow_context_u32(request, "/coder/workflow_loop/round").unwrap_or(1);
    let max_rounds = workflow_context_u32(request, "/coder/workflow_loop/max_rounds")
        .unwrap_or(1)
        .max(1);
    if round >= max_rounds {
        let reason = format!(
            "{pending_count} queued user requirement(s) remain unapplied and the maximum round budget was reached"
        );
        return Some(BoundedWorkflowDecision {
            decision: "blocked".to_owned(),
            summary: "The workflow cannot claim completion while newly queued user requirements remain unapplied.".to_owned(),
            improvements: Vec::new(),
            expected_gain: "none".to_owned(),
            blockers: vec![reason.clone()],
            stop_reason: Some(reason),
        });
    }
    Some(BoundedWorkflowDecision {
        decision: "continue".to_owned(),
        summary: format!(
            "Continue so the Executor can consume {pending_count} newly queued user requirement(s)."
        ),
        improvements: vec![
            "Apply the queued user requirements and verify their observable behavior.".to_owned(),
        ],
        expected_gain: "high".to_owned(),
        blockers: Vec::new(),
        stop_reason: None,
    })
}

fn workflow_planner_messages(request: &HarnessRunRequest) -> Vec<Value> {
    let system = request
        .backend_context
        .pointer("/coder/agent/system")
        .and_then(Value::as_str)
        .unwrap_or("You are the read-only Workflow Planner.");
    let plan = request
        .backend_context
        .pointer("/coder/plan_context/plan_draft");
    let input = json!({
        "original_user_request": request.backend_context
            .pointer("/coder/plan_context/original_user_request")
            .cloned()
            .unwrap_or_else(|| Value::String(request.task.clone())),
        "plan": {
            "goal": plan.and_then(|value| value.get("goal")).cloned().unwrap_or(Value::Null),
            "scope": plan.and_then(|value| value.get("scope")).cloned().unwrap_or(Value::Null),
            "acceptance_criteria": plan.and_then(|value| value.get("acceptance_criteria")).cloned().unwrap_or(Value::Null),
            "risks": plan.and_then(|value| value.get("risks")).cloned().unwrap_or(Value::Null)
        },
        "verifier_feedback": request.backend_context
            .pointer("/coder/plan_context/workflow_feedback")
            .cloned()
            .unwrap_or(Value::Null),
        "loop_budget": request.backend_context
            .pointer("/coder/workflow_loop")
            .cloned()
            .unwrap_or(Value::Null)
    });
    vec![
        json!({
            "role": "system",
            "content": format!(
                "{system}\n\nJudge whether the verified result already satisfies the user's explicit and qualitative goals. You own the finish-or-improve decision but never edit files. Map each completion claim to direct evidence; a criterion copied from the plan is not evidence. A smoke-test PASS proves basic correctness, not responsiveness, a representative user flow, or an explicit qualitative target such as fun, polished, or production-ready. If qualitative evidence contains no task-specific review or playtest, request one focused review/refinement with observable evidence; never repeat that direction. Treat continuation as a budgeted investment: continue only when the expected quality gain clearly exceeds the cost of another execution and verification round. Once the acceptance criteria are met, finish even when optional enhancements remain. Continue only for 1-3 concrete, observable improvements with medium or high expected gain. Do not continue for generic polish, speculative work, repeated advice, or low marginal gain. Respect round and progress budgets. A verifier failure may continue only when the repair is actionable; blocked means unmet work needs external input or the bounded loop cannot make progress. Return JSON only with exactly these fields: decision (finish|continue|blocked), summary, improvements (array, at most 3), expected_gain (high|medium|low|none), blockers (array). Do not reveal chain-of-thought."
            )
        }),
        json!({
            "role": "user",
            "content": format!(
                "Make the next bounded workflow decision from this compact evidence:\n{}",
                input
            )
        }),
    ]
}

fn parse_model_decision(content: &str) -> Result<ModelWorkflowDecision, String> {
    let trimmed = content.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    serde_json::from_str(json).map_err(|error| error.to_string())
}

fn enforce_bounded_decision(
    request: &HarnessRunRequest,
    model: ModelWorkflowDecision,
) -> BoundedWorkflowDecision {
    let mut decision = BoundedWorkflowDecision {
        decision: model.decision.trim().to_ascii_lowercase(),
        summary: compact_text(&model.summary, 600),
        improvements: normalize_items(model.improvements, WORKFLOW_PLANNER_MAX_IMPROVEMENTS),
        expected_gain: model.expected_gain.trim().to_ascii_lowercase(),
        blockers: normalize_items(model.blockers, WORKFLOW_PLANNER_MAX_IMPROVEMENTS),
        stop_reason: None,
    };
    if decision.summary.is_empty() {
        decision.summary = "Workflow Planner returned a bounded control decision.".to_owned();
    }
    if !matches!(
        decision.expected_gain.as_str(),
        "high" | "medium" | "low" | "none"
    ) {
        decision.expected_gain = "none".to_owned();
    }
    let source_node = workflow_context_str(
        request,
        "/coder/plan_context/workflow_feedback/source_node_id",
    );
    let source_signal =
        workflow_context_str(request, "/coder/plan_context/workflow_feedback/signal");
    let verifier_completed = source_node == "verifier" && source_signal == "completed";
    let verified_completion = verifier_completed && !qualitative_executor_was_interrupted(request);
    if source_node != "verifier" {
        return apply_stop_gate(
            decision,
            verified_completion,
            "the decision did not follow verifier evidence",
        );
    }
    if !matches!(
        decision.decision.as_str(),
        "finish" | "continue" | "blocked"
    ) {
        return apply_stop_gate(
            decision,
            verified_completion,
            "the Planner returned an unsupported decision",
        );
    }
    if decision.decision == "finish"
        && verifier_completed
        && qualitative_executor_was_interrupted(request)
    {
        return interrupted_executor_decision(request, decision);
    }
    if decision.decision == "finish" && !verified_completion {
        return apply_stop_gate(
            decision,
            false,
            "finish requires completed verifier evidence",
        );
    }
    if decision.decision == "blocked" {
        if verified_completion {
            return apply_stop_gate(
                decision,
                true,
                "verified work should not be blocked for optional refinement",
            );
        }
        if decision.blockers.is_empty() {
            decision.blockers.push(
                "Workflow Planner reported a blocker without a repairable next step.".to_owned(),
            );
        }
        return decision;
    }
    if decision.decision != "continue" {
        decision.improvements.clear();
        return decision;
    }

    let round = workflow_context_u32(request, "/coder/workflow_loop/round").unwrap_or(1);
    let max_rounds = workflow_context_u32(request, "/coder/workflow_loop/max_rounds")
        .unwrap_or(1)
        .max(1);
    if round >= max_rounds {
        return apply_stop_gate(
            decision,
            verified_completion,
            "the maximum round budget was reached",
        );
    }
    let executor_evidence = request
        .backend_context
        .pointer("/coder/workflow_loop/executor_evidence_this_round")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if round > 1 && !executor_evidence {
        return apply_stop_gate(
            decision,
            verified_completion,
            "the previous refinement produced no executor change evidence",
        );
    }
    if decision.improvements.is_empty() {
        return apply_stop_gate(
            decision,
            verified_completion,
            "continue requires a concrete observable improvement",
        );
    }
    if !matches!(decision.expected_gain.as_str(), "high" | "medium") {
        return apply_stop_gate(
            decision,
            verified_completion,
            "the expected marginal gain was below the continuation threshold",
        );
    }
    let fingerprint = improvement_fingerprint(&decision.improvements);
    if previous_improvement_fingerprints(request).contains(&fingerprint) {
        return apply_stop_gate(
            decision,
            verified_completion,
            "the proposed improvement repeated a previous Planner direction",
        );
    }
    decision
}

fn completion_is_claimable(request: &HarnessRunRequest) -> bool {
    workflow_context_str(
        request,
        "/coder/plan_context/workflow_feedback/source_node_id",
    ) == "verifier"
        && workflow_context_str(request, "/coder/plan_context/workflow_feedback/signal")
            == "completed"
        && !qualitative_executor_was_interrupted(request)
}

fn qualitative_executor_was_interrupted(request: &HarnessRunRequest) -> bool {
    qualitative_review_requested(request)
        && workflow_context_str(request, "/coder/workflow_loop/executor_evidence_summary")
            .contains(INTERRUPTED_EXECUTOR_MARKER)
}

fn interrupted_executor_decision(
    request: &HarnessRunRequest,
    mut decision: BoundedWorkflowDecision,
) -> BoundedWorkflowDecision {
    let round = workflow_context_u32(request, "/coder/workflow_loop/round").unwrap_or(1);
    let max_rounds = workflow_context_u32(request, "/coder/workflow_loop/max_rounds")
        .unwrap_or(1)
        .max(1);
    let repeated =
        previous_improvement_fingerprints(request).contains(&improvement_fingerprint(&[
            COMPLETE_INTERRUPTED_EXECUTOR.to_owned(),
        ]));
    if round >= max_rounds || repeated {
        let reason = if repeated {
            "the qualitative Executor repeatedly reached its turn limit without a final self-review"
        } else {
            "the qualitative Executor reached its turn limit without a final self-review on the last workflow round"
        };
        decision.decision = "blocked".to_owned();
        decision.summary = "The workflow cannot claim qualitative completion from smoke evidence after the Executor stopped without a final response.".to_owned();
        decision.improvements.clear();
        decision.expected_gain = "none".to_owned();
        decision.blockers = vec![reason.to_owned()];
        decision.stop_reason = Some(reason.to_owned());
        return decision;
    }
    decision.decision = "continue".to_owned();
    decision.summary = "Continue once because the qualitative Executor changed files but stopped before its final self-review.".to_owned();
    decision.improvements = vec![COMPLETE_INTERRUPTED_EXECUTOR.to_owned()];
    decision.expected_gain = "high".to_owned();
    decision.blockers.clear();
    decision.stop_reason = None;
    decision
}

fn bounded_stop_decision(reason: String) -> BoundedWorkflowDecision {
    let reason = compact_text(&reason, 600);
    BoundedWorkflowDecision {
        decision: "blocked".to_owned(),
        summary: "Workflow Planner could not produce the required live quality decision."
            .to_owned(),
        improvements: Vec::new(),
        expected_gain: "none".to_owned(),
        blockers: vec![reason.clone()],
        stop_reason: Some(reason),
    }
}

fn budget_exhausted_decision() -> BoundedWorkflowDecision {
    BoundedWorkflowDecision {
        decision: "blocked".to_owned(),
        summary: "The workflow stopped before another model call because its shared token budget was exhausted.".to_owned(),
        improvements: Vec::new(),
        expected_gain: "none".to_owned(),
        blockers: vec!["workflow token budget was exhausted".to_owned()],
        stop_reason: Some("the workflow token budget was exhausted".to_owned()),
    }
}

fn apply_stop_gate(
    mut decision: BoundedWorkflowDecision,
    verified_completion: bool,
    reason: &str,
) -> BoundedWorkflowDecision {
    let reason = compact_text(reason, 600);
    decision.stop_reason = Some(reason.clone());
    decision.improvements.clear();
    decision.expected_gain = "none".to_owned();
    if verified_completion {
        decision.decision = "finish".to_owned();
        decision.blockers.clear();
        decision.summary = format!("Accepted verified completion because {reason}.");
    } else {
        decision.decision = "blocked".to_owned();
        decision.blockers = vec![reason.clone()];
        decision.summary = format!("Stopped the bounded workflow because {reason}.");
    }
    decision
}

fn workflow_planner_result(
    request: &HarnessRunRequest,
    decision: BoundedWorkflowDecision,
    provider_trace: Option<PlannerProviderTrace>,
    token_budget: Option<RunTokenBudgetSnapshot>,
) -> HarnessRunResult {
    let implementation = if provider_trace.is_some() {
        "provider-backed-bounded-planner"
    } else {
        "deterministic-bounded-gate"
    };
    let mut report = if decision.decision == "blocked" {
        FinalReport::blocked(
            decision.summary.clone(),
            decision
                .blockers
                .first()
                .cloned()
                .unwrap_or_else(|| "workflow Planner blocked".to_owned()),
        )
    } else {
        FinalReport::completed(decision.summary.clone())
    };
    report.checks.push(format!(
        "workflow planner decision: {} (expected gain: {})",
        decision.decision, decision.expected_gain
    ));
    report.checks.extend(
        decision
            .improvements
            .iter()
            .map(|item| format!("planned improvement: {item}")),
    );
    if let Some(reason) = decision.stop_reason.as_deref() {
        report.checks.push(format!("workflow stop gate: {reason}"));
    }
    for blocker in decision.blockers.iter().skip(1) {
        report.blockers.push(blocker.clone());
    }
    let readiness = if decision.decision == "finish" {
        "finished"
    } else {
        decision.decision.as_str()
    };
    HarnessRunResult {
        status: decision.decision.clone(),
        report: Some(report),
        events: vec![
            HarnessRunEvent::new(
                "planner.workflow_decision",
                json!({
                    "backend": "planner-model",
                    "implementation": implementation,
                    "node_id": request.node_id,
                    "agent_id": request.agent_id,
                    "harness_id": request.harness_id,
                    "decision": decision.decision,
                    "summary": decision.summary,
                    "improvements": decision.improvements,
                    "expected_gain": decision.expected_gain,
                    "blockers": decision.blockers,
                    "stop_reason": decision.stop_reason,
                    "round": workflow_context_u32(request, "/coder/workflow_loop/round"),
                    "max_rounds": workflow_context_u32(request, "/coder/workflow_loop/max_rounds"),
                    "provider_trace": provider_trace,
                    "token_budget": token_budget.map(RunTokenBudgetSnapshot::as_json)
                }),
            ),
            HarnessRunEvent::new(
                "planner.readiness.changed",
                json!({"backend": "planner-model", "readiness": readiness}),
            ),
        ],
    }
}

fn previous_improvement_fingerprints(request: &HarnessRunRequest) -> BTreeSet<String> {
    request
        .backend_context
        .pointer("/coder/workflow_loop/previous_improvements")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_array)
        .map(|items| {
            let items = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            improvement_fingerprint(&items)
        })
        .filter(|value| !value.is_empty())
        .collect()
}

fn improvement_fingerprint(items: &[String]) -> String {
    let mut normalized = items
        .iter()
        .map(|item| {
            item.chars()
                .filter(|ch| ch.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.join("|")
}

fn normalize_items(items: Vec<String>, limit: usize) -> Vec<String> {
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .map(|item| compact_text(&item, 360))
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert(item.to_ascii_lowercase()))
        .take(limit)
        .collect()
}

fn compact_text(value: &str, max_chars: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn workflow_context_str<'a>(request: &'a HarnessRunRequest, pointer: &str) -> &'a str {
    request
        .backend_context
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn workflow_context_u32(request: &HarnessRunRequest, pointer: &str) -> Option<u32> {
    request
        .backend_context
        .pointer(pointer)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn workflow_planner_stream_idle_timeout(request: &HarnessRunRequest) -> Duration {
    Duration::from_millis(
        request
            .backend_context
            .pointer("/coder/agent/runtime/stream_idle_timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(90_000)
            .clamp(10_000, 600_000),
    )
}

fn qualitative_review_requested(request: &HarnessRunRequest) -> bool {
    let original_request =
        workflow_context_str(request, "/coder/plan_context/original_user_request");
    let goal = workflow_context_str(request, "/coder/plan_context/plan_draft/goal");
    let acceptance = request
        .backend_context
        .pointer("/coder/plan_context/plan_draft/acceptance_criteria")
        .or_else(|| {
            request
                .backend_context
                .pointer("/coder/plan_context/acceptance_criteria")
        })
        .map(Value::to_string)
        .unwrap_or_default();
    let text = format!("{original_request}\n{goal}\n{acceptance}").to_lowercase();
    let words = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<BTreeSet<_>>();
    let word_match = [
        "fun",
        "game",
        "polish",
        "polished",
        "engaging",
        "delightful",
        "beautiful",
        "refine",
        "improve",
        "better",
    ]
    .iter()
    .any(|marker| words.contains(marker));
    let phrase_match = [
        "production-ready",
        "production ready",
        "high quality",
        "higher quality",
        "best possible",
        "\u{6e38}\u{620f}",
        "\u{597d}\u{73a9}",
        "\u{7cbe}\u{81f4}",
        "\u{9ad8}\u{8d28}\u{91cf}",
        "\u{751f}\u{4ea7}\u{7ea7}",
        "\u{66f4}\u{597d}",
        "\u{4f18}\u{5316}",
        "\u{5b8c}\u{5584}",
        "\u{7f8e}\u{89c2}",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    word_match || phrase_match
}

#[cfg(test)]
mod tests {
    use coder_core::RunId;

    use super::*;

    fn request(signal: &str, round: u32, max_rounds: u32, evidence: bool) -> HarnessRunRequest {
        HarnessRunRequest {
            run_id: RunId::from_string("bounded-planner-test"),
            workflow_id: "planner-led".to_owned(),
            node_id: "workflow-planner".to_owned(),
            agent_id: "workflow-planner".to_owned(),
            harness_id: "workflow-planner".to_owned(),
            repo_root: ".".to_owned(),
            task: "decide".to_owned(),
            backend_context: json!({
                "coder": {
                    "plan_context": {"workflow_feedback": {
                        "source_node_id": "verifier",
                        "signal": signal
                    }},
                    "workflow_loop": {
                        "round": round,
                        "max_rounds": max_rounds,
                        "executor_evidence_this_round": evidence,
                        "executor_evidence_summary": "",
                        "previous_improvements": []
                    }
                }
            }),
        }
    }

    fn model_decision(decision: &str, gain: &str, improvements: &[&str]) -> ModelWorkflowDecision {
        ModelWorkflowDecision {
            decision: decision.to_owned(),
            summary: "bounded test decision".to_owned(),
            improvements: improvements
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            expected_gain: gain.to_owned(),
            blockers: Vec::new(),
        }
    }

    #[test]
    fn verified_simple_task_finishes_without_an_extra_round() {
        let decision = enforce_bounded_decision(
            &request("completed", 1, 3, true),
            model_decision("finish", "none", &[]),
        );
        assert_eq!(decision.decision, "finish");
        assert!(decision.stop_reason.is_none());
    }

    #[test]
    fn medium_gain_concrete_improvement_can_continue() {
        let decision = enforce_bounded_decision(
            &request("completed", 1, 3, true),
            model_decision(
                "continue",
                "medium",
                &["Add a visible restart control and verify it resets state"],
            ),
        );
        assert_eq!(decision.decision, "continue");
        assert!(decision.stop_reason.is_none());
    }

    #[test]
    fn qualitative_interrupted_executor_forces_one_completion_round() {
        let mut request = request("completed", 1, 3, true);
        request.backend_context["coder"]["plan_context"]["original_user_request"] =
            json!("Build a polished game.");
        request.backend_context["coder"]["workflow_loop"]["executor_evidence_summary"] =
            json!(format!("check: {INTERRUPTED_EXECUTOR_MARKER}"));

        let decision = enforce_bounded_decision(&request, model_decision("finish", "none", &[]));

        assert_eq!(decision.decision, "continue");
        assert_eq!(decision.expected_gain, "high");
        assert_eq!(decision.improvements, vec![COMPLETE_INTERRUPTED_EXECUTOR]);
    }

    #[test]
    fn repeated_qualitative_executor_interruption_is_reported_as_blocked() {
        let mut request = request("completed", 2, 3, true);
        request.backend_context["coder"]["plan_context"]["original_user_request"] =
            json!("Build a polished game.");
        request.backend_context["coder"]["workflow_loop"]["executor_evidence_summary"] =
            json!(format!("check: {INTERRUPTED_EXECUTOR_MARKER}"));
        request.backend_context["coder"]["workflow_loop"]["previous_improvements"] =
            json!([[COMPLETE_INTERRUPTED_EXECUTOR]]);

        let decision = enforce_bounded_decision(&request, model_decision("finish", "none", &[]));

        assert_eq!(decision.decision, "blocked");
        assert!(decision.blockers[0].contains("repeatedly reached its turn limit"));
    }

    #[test]
    fn closed_task_can_accept_verified_writes_after_executor_turn_limit() {
        let mut request = request("completed", 1, 3, true);
        request.backend_context["coder"]["plan_context"]["original_user_request"] =
            json!("Create README.md with the supplied text.");
        request.backend_context["coder"]["workflow_loop"]["executor_evidence_summary"] =
            json!(format!("check: {INTERRUPTED_EXECUTOR_MARKER}"));

        let decision = enforce_bounded_decision(&request, model_decision("finish", "none", &[]));

        assert_eq!(decision.decision, "finish");
    }

    #[test]
    fn planner_prompt_receives_compact_executor_evidence() {
        let mut request = request("completed", 1, 3, true);
        request.backend_context["coder"]["workflow_loop"]["executor_evidence_summary"] =
            json!("changed: src/game.rs; check: task-specific playtest passed");

        let messages = workflow_planner_messages(&request);
        let user_message = messages[1]["content"].as_str().unwrap();

        assert!(user_message.contains("task-specific playtest passed"));
        assert!(user_message.contains("src/game.rs"));
    }

    #[test]
    fn workflow_planner_honors_validated_stream_idle_timeout_range() {
        let mut request = request("completed", 1, 3, true);
        assert_eq!(
            workflow_planner_stream_idle_timeout(&request),
            Duration::from_millis(90_000)
        );

        request.backend_context["coder"]["agent"]["runtime"]["stream_idle_timeout_ms"] =
            json!(600_000);
        assert_eq!(
            workflow_planner_stream_idle_timeout(&request),
            Duration::from_millis(600_000)
        );
    }

    #[test]
    fn final_round_forces_verified_completion_to_finish() {
        let decision = enforce_bounded_decision(
            &request("completed", 3, 3, true),
            model_decision("continue", "high", &["Add another mechanic"]),
        );
        assert_eq!(decision.decision, "finish");
        assert!(decision.stop_reason.unwrap().contains("maximum round"));
    }

    #[test]
    fn final_round_blocks_when_verification_still_fails() {
        let decision = enforce_bounded_decision(
            &request("failed", 3, 3, true),
            model_decision("continue", "high", &["Repair the failing interaction"]),
        );
        assert_eq!(decision.decision, "blocked");
        assert!(decision.blockers[0].contains("maximum round"));
    }

    #[test]
    fn low_marginal_gain_does_not_extend_verified_work() {
        let decision = enforce_bounded_decision(
            &request("completed", 1, 3, true),
            model_decision("continue", "low", &["Tweak minor spacing"]),
        );
        assert_eq!(decision.decision, "finish");
        assert!(decision.stop_reason.unwrap().contains("marginal gain"));
    }

    #[test]
    fn repeated_improvement_does_not_extend_verified_work() {
        let mut request = request("completed", 2, 3, true);
        request.backend_context["coder"]["workflow_loop"]["previous_improvements"] =
            json!([["Add keyboard controls"]]);
        let decision = enforce_bounded_decision(
            &request,
            model_decision("continue", "high", &["Add keyboard controls"]),
        );
        assert_eq!(decision.decision, "finish");
        assert!(decision.stop_reason.unwrap().contains("repeated"));
    }

    #[test]
    fn no_executor_progress_stops_a_second_refinement() {
        let decision = enforce_bounded_decision(
            &request("completed", 2, 3, false),
            model_decision("continue", "high", &["Add more feedback"]),
        );
        assert_eq!(decision.decision, "finish");
        assert!(decision
            .stop_reason
            .unwrap()
            .contains("no executor change evidence"));
    }

    #[test]
    fn provider_failure_does_not_turn_unreviewed_quality_into_success() {
        let mut request = request("completed", 1, 3, true);
        request.backend_context["coder"]["plan_context"]["original_user_request"] =
            json!("Build a more fun strategy game.");

        let decision = bounded_stop_decision("provider decision unavailable: HTTP 402".to_owned());

        assert_eq!(decision.decision, "blocked");
        assert!(decision.blockers[0].contains("HTTP 402"));
        assert!(decision.stop_reason.unwrap().contains("HTTP 402"));
    }

    #[test]
    fn quality_router_skips_closed_tasks_and_selects_open_ended_goals() {
        let mut closed = request("completed", 1, 3, true);
        closed.backend_context["coder"]["plan_context"]["original_user_request"] =
            json!("Create math functions and README.md with the supplied text.");
        assert!(!qualitative_review_requested(&closed));

        closed.backend_context["coder"]["plan_context"]["original_user_request"] =
            json!("Build a more fun strategy game.");
        assert!(qualitative_review_requested(&closed));
    }

    #[test]
    fn quality_router_handles_multilingual_goals_without_routing_plain_closed_tasks() {
        for (task, expected) in [
            ("Create README.md with the supplied text.", false),
            (
                "\u{521b}\u{5efa} README.md\u{ff0c}\u{5185}\u{5bb9}\u{4f7f}\u{7528}\u{5df2}\u{63d0}\u{4f9b}\u{7684}\u{6587}\u{672c}\u{3002}",
                false,
            ),
            ("Build a production-ready service.", true),
            (
                "\u{505a}\u{4e00}\u{4e2a}\u{6bd4}\u{4e0a}\u{6b21}\u{66f4}\u{597d}\u{73a9}\u{7684}\u{690d}\u{7269}\u{5927}\u{6218}\u{50f5}\u{5c38}\u{6e38}\u{620f}\u{3002}",
                true,
            ),
        ] {
            let mut request = request("completed", 1, 3, true);
            request.backend_context["coder"]["plan_context"]["original_user_request"] =
                json!(task);
            assert_eq!(qualitative_review_requested(&request), expected, "{task}");
        }

        let mut criteria = request("completed", 1, 3, true);
        criteria.backend_context["coder"]["plan_context"]["original_user_request"] =
            json!("Build the requested interface.");
        criteria.backend_context["coder"]["plan_context"]["plan_draft"] = json!({
            "goal": "Build the requested interface.",
            "acceptance_criteria": [
                "\u{754c}\u{9762}\u{7cbe}\u{81f4}\u{ff0c}\u{4e14}\u{4e3b}\u{6d41}\u{79fb}\u{52a8}\u{7aef}\u{65e0}\u{5e03}\u{5c40}\u{6ea2}\u{51fa}\u{3002}"
            ]
        });
        assert!(qualitative_review_requested(&criteria));
    }

    #[test]
    fn planner_prompt_does_not_equate_smoke_pass_with_qualitative_success() {
        let request = request("completed", 1, 3, true);
        let messages = workflow_planner_messages(&request);
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("smoke-test PASS proves basic correctness"));
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("one focused review/refinement"));
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("criterion copied from the plan is not evidence"));
    }
}
