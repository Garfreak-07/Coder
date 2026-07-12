use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::{de::Error as _, Deserialize, Deserializer};
use serde_json::Value;

use crate::api_types::{
    MemoryProposalDraft, PlanDraft, PlanExecutionMode, PlanReviewMode, PlannerArtifact,
    PlannerConversationEngine, PlannerConversationRequest, PlannerConversationResponse,
    PlannerReadiness, PlannerRuntimeContext,
};
use crate::planner_provider_runtime::LivePlannerMessage;

pub(crate) fn normalize_planner_mode(value: Option<&str>) -> String {
    if value
        .map(|item| item.trim().eq_ignore_ascii_case("work"))
        .unwrap_or(false)
    {
        "work".to_owned()
    } else {
        "discuss".to_owned()
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DeterministicPlannerConversationEngine;

#[async_trait]
impl PlannerConversationEngine for DeterministicPlannerConversationEngine {
    async fn respond(
        &self,
        request: PlannerConversationRequest,
    ) -> Result<PlannerConversationResponse, String> {
        Ok(deterministic_planner_response(&request, None))
    }
}

pub(crate) fn planner_system_prompt(runtime: &PlannerRuntimeContext) -> String {
    const CONTRACT: &str = r#"Planner contract:
- You are Coder Planner.
- Chat is planning and conversation only.
- You do not edit files or run commands in chat.
- You prepare the task and decide whether it is ready for Start Work.
- Infer the task domain, implicit user expectations, and an observable quality bar when that is safer than asking about non-blocking details.
- Separate known facts from assumptions. Use the bounded read-only repository tools for relevant facts when a repository is bound; never invent repository facts.
- Prefer the smallest complete plan. Include dependencies, likely challenges, trade-offs, and task-specific acceptance criteria only when they affect execution.
- Make every material requested or inferred behavior in goal/scope traceable to an observable acceptance criterion. A generic build/pass criterion is insufficient when behavior matters.
- Make qualitative criteria falsifiable. Do not repeat bare adjectives such as functional, enjoyable, clean, responsive, polished, or deliverable; name a representative user flow plus the relevant state, viewport, or observable evidence that would disprove completion.
- Classify plan_draft.execution_mode as read_only, may_write, or must_write and plan_draft.review_mode as standard or qualitative. These typed fields control execution and review; do not omit them.
- Ask only questions whose answers materially change scope, safety, or the implementation direction.
- If the user says to use your judgement or decide details yourself, treat optional product and design choices as delegated: keep open_questions empty and normally set ready_for_start_work=true.
- When the user asks you to do work, explain briefly that execution happens after Start Work.
- When ready, set ready_for_start_work=true.
- Do not ask the user to switch modes.
- Do not mention Discuss mode or Work mode.
- Do not produce long internal plans unless asked.
- Keep assistant_message under 250 visible words when possible and always under 600 words.
- Return only one compact JSON object matching the strict_output_contract in the supplied planner context. Do not wrap it in markdown fences."#;
    format!(
        "{}\n\n{}\n\nRuntime boundary:\n- workflow_id: {}\n- workflow_name: {}\n- node_id: {}\n- agent_id: {}\n- harness_id: {}\n- repository_tools: bounded read-only snapshot when a repository is bound\n- terminal: disabled\n- file_editor: disabled\n- command_execution: disabled\n- network_tools: disabled\n- side effects: denied\n\nRepository inspection is not task execution. Never claim files changed or commands ran during Planner Chat.",
        runtime.agent.system,
        CONTRACT,
        runtime.workflow_id,
        runtime.workflow_name,
        runtime.node_id,
        runtime.agent_id,
        runtime.harness_id
    )
}

pub(crate) fn planner_provider_setup_required_response(
    message: String,
) -> PlannerConversationResponse {
    PlannerConversationResponse {
        assistant_message: message,
        plan_draft: None,
        readiness: PlannerReadiness::Blocked,
        open_questions: vec![
            "Open Settings, save a provider API key, then send the Planner message again."
                .to_owned(),
        ],
        acceptance_criteria: Vec::new(),
        risks: Vec::new(),
        suggested_mode: "discuss".to_owned(),
        should_start_workflow: false,
        artifacts: Vec::new(),
        response_truncated: false,
        large_artifacts: false,
        provider_trace: None,
    }
}

pub(crate) fn planner_provider_unavailable_response(
    request: &PlannerConversationRequest,
    error: &str,
) -> PlannerConversationResponse {
    let mut response = deterministic_planner_response(request, None);
    let error = single_line_preview(error, 360);
    response.assistant_message = format!(
        "Planner provider is unavailable: {error}. No work was started. Retry after provider access is restored."
    );
    response.readiness = PlannerReadiness::Blocked;
    response.open_questions =
        vec!["Restore provider access or retry when the provider is available.".to_owned()];
    response.suggested_mode = "discuss".to_owned();
    response.should_start_workflow = false;
    response.provider_trace = None;
    response
}

pub(crate) fn concise_plan_summary(plan: Option<&PlanDraft>, fallback_message: &str) -> String {
    let Some(plan) = plan else {
        return single_line_preview(fallback_message, 240);
    };
    let mut parts = Vec::new();
    if !plan.goal.trim().is_empty() {
        parts.push(format!("Goal: {}", single_line_preview(&plan.goal, 180)));
    }
    if !plan.scope.is_empty() {
        parts.push(format!(
            "Scope: {}",
            plan.scope
                .iter()
                .take(3)
                .map(|item| single_line_preview(item, 80))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !plan.acceptance_criteria.is_empty() {
        parts.push(format!(
            "Checks: {}",
            plan.acceptance_criteria
                .iter()
                .take(2)
                .map(|item| single_line_preview(item, 100))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    if parts.is_empty() {
        single_line_preview(fallback_message, 240)
    } else {
        parts.join(" ")
    }
}

pub(crate) fn deterministic_planner_response(
    request: &PlannerConversationRequest,
    model_message: Option<LivePlannerMessage>,
) -> PlannerConversationResponse {
    let model_envelope = model_message
        .as_ref()
        .and_then(|message| parse_model_planner_envelope(&message.content));
    let mode = normalize_planner_mode(Some(&request.mode));
    let work_like = message_looks_like_work(&request.message) || request.current_plan.is_some();
    if mode == "discuss" && !work_like {
        let raw_message = model_envelope
            .as_ref()
            .map(|envelope| envelope.assistant_message.clone())
            .filter(|message| !message.trim().is_empty())
            .or_else(|| model_message.as_ref().map(|message| message.content.clone()))
            .unwrap_or_else(|| {
                "I can discuss that. If you want repository work later, I will first turn it into a scoped plan and keep execution behind Start Work.".to_owned()
            });
        let shaped = shape_planner_assistant_message(
            raw_message,
            PlannerReadiness::Casual,
            None,
            live_message_was_length_truncated(model_message.as_ref()),
        );
        return PlannerConversationResponse {
            assistant_message: shaped.assistant_message,
            plan_draft: request.current_plan.clone(),
            readiness: PlannerReadiness::Casual,
            open_questions: Vec::new(),
            acceptance_criteria: Vec::new(),
            risks: Vec::new(),
            suggested_mode: "discuss".to_owned(),
            should_start_workflow: false,
            artifacts: shaped.artifacts,
            response_truncated: shaped.response_truncated,
            large_artifacts: shaped.large_artifacts,
            provider_trace: model_message
                .as_ref()
                .map(|message| message.provider_trace.clone()),
        };
    }

    let deterministic_plan = planner_plan_draft(request);
    let plan = model_envelope
        .as_ref()
        .and_then(|envelope| envelope.plan_draft.clone())
        .map(|model_plan| {
            let explicit_paths = extract_affected_paths(&request.message);
            let explicit_criteria = extract_acceptance_criteria(&request.message);
            let explicit_risks = extract_risks(&request.message);
            merge_model_plan(
                deterministic_plan.clone(),
                model_plan,
                &explicit_paths,
                &explicit_criteria,
                &explicit_risks,
                (request.current_plan.is_some()
                    && message_is_pure_plan_confirmation(&request.message))
                    || message_explicitly_read_only(&request.message),
                (request.current_plan.is_some()
                    && message_is_pure_plan_confirmation(&request.message))
                    || message_explicitly_qualitative(&request.message),
            )
        })
        .unwrap_or(deterministic_plan);
    let planning_only = message_requests_planning_only(&request.message) && mode != "work";
    let readiness = if planning_only
        || model_envelope
            .as_ref()
            .is_some_and(|envelope| !envelope.ready_for_start_work)
    {
        PlannerReadiness::NeedsClarification
    } else if plan.open_questions.is_empty() {
        PlannerReadiness::Ready
    } else {
        PlannerReadiness::NeedsClarification
    };
    let mut raw_message = model_envelope
        .as_ref()
        .map(|envelope| envelope.assistant_message.clone())
        .filter(|message| !message.trim().is_empty())
        .or_else(|| {
            model_message
                .as_ref()
                .map(|message| message.content.clone())
        })
        .unwrap_or_else(|| {
            if planning_only {
                planner_plan_overview_message(&plan)
            } else {
                deterministic_planner_message(&mode, &plan, readiness, request.confirmed)
            }
        });
    if planning_only && planner_message_lacks_plan_details(&raw_message) {
        raw_message = planner_plan_overview_message(&plan);
    }
    let shaped = shape_planner_assistant_message(
        raw_message,
        readiness,
        Some(&plan),
        live_message_was_length_truncated(model_message.as_ref()),
    );

    PlannerConversationResponse {
        assistant_message: shaped.assistant_message,
        open_questions: plan.open_questions.clone(),
        acceptance_criteria: plan.acceptance_criteria.clone(),
        risks: plan.risks.clone(),
        suggested_mode: if readiness == PlannerReadiness::Ready {
            "work".to_owned()
        } else {
            "discuss".to_owned()
        },
        should_start_workflow: false,
        readiness,
        plan_draft: Some(plan),
        artifacts: shaped.artifacts,
        response_truncated: shaped.response_truncated,
        large_artifacts: shaped.large_artifacts,
        provider_trace: model_message
            .as_ref()
            .map(|message| message.provider_trace.clone()),
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ModelPlannerEnvelope {
    #[serde(default)]
    assistant_message: String,
    #[serde(default)]
    ready_for_start_work: bool,
    #[serde(
        default,
        alias = "plan",
        deserialize_with = "deserialize_model_plan_draft"
    )]
    plan_draft: Option<PlanDraft>,
}

fn deserialize_model_plan_draft<'de, D>(deserializer: D) -> Result<Option<PlanDraft>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(mut value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    if let Some(goal) = value.get_mut("goal") {
        if let Some(items) = goal.as_array() {
            let normalized = items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
                .join("; ");
            *goal = Value::String(normalized);
        }
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(D::Error::custom)
}

fn parse_model_planner_envelope(content: &str) -> Option<ModelPlannerEnvelope> {
    let trimmed = content.trim();
    let json = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|body| body.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed);
    serde_json::from_str(json).ok()
}

fn merge_model_plan(
    mut deterministic: PlanDraft,
    model: PlanDraft,
    explicit_paths: &[String],
    explicit_criteria: &[String],
    explicit_risks: &[String],
    preserve_execution_mode: bool,
    preserve_review_mode: bool,
) -> PlanDraft {
    if !model.goal.trim().is_empty() {
        deterministic.goal = single_line_preview(&model.goal, 600);
    }
    if !preserve_execution_mode {
        deterministic.execution_mode = model.execution_mode;
    }
    if !preserve_review_mode {
        deterministic.review_mode = model.review_mode;
    }
    merge_plan_items_preserving_explicit(&mut deterministic.scope, model.scope, explicit_paths, 12);
    merge_plan_items(&mut deterministic.non_goals, model.non_goals, 12);
    merge_plan_items(&mut deterministic.assumptions, model.assumptions, 12);
    merge_plan_items(&mut deterministic.steps, model.steps, 16);
    merge_plan_items_preserving_explicit(
        &mut deterministic.affected_paths,
        model.affected_paths,
        explicit_paths,
        24,
    );
    let mut model_criteria = bounded_plan_items(model.acceptance_criteria, 16);
    if !model_criteria.is_empty() {
        if explicit_criteria.is_empty() {
            deterministic.acceptance_criteria = model_criteria;
        } else {
            let mut merged = explicit_criteria.to_vec();
            merged.append(&mut model_criteria);
            deterministic.acceptance_criteria = bounded_plan_items(merged, 16);
        }
    }
    merge_plan_items_preserving_explicit(&mut deterministic.risks, model.risks, explicit_risks, 12);
    deterministic.open_questions = bounded_plan_items(model.open_questions, 8);
    deterministic
}

fn merge_plan_items(target: &mut Vec<String>, source: Vec<String>, max_items: usize) {
    let items = bounded_plan_items(source, max_items);
    if !items.is_empty() {
        *target = items;
    }
}

fn merge_plan_items_preserving_explicit(
    target: &mut Vec<String>,
    mut source: Vec<String>,
    explicit: &[String],
    max_items: usize,
) {
    if source.is_empty() {
        return;
    }
    if explicit.is_empty() {
        *target = bounded_plan_items(source, max_items);
        return;
    }
    let mut merged = explicit.to_vec();
    merged.append(&mut source);
    *target = bounded_plan_items(merged, max_items);
}

fn bounded_plan_items(source: Vec<String>, max_items: usize) -> Vec<String> {
    unique_strings(source)
        .into_iter()
        .map(|item| single_line_preview(&item, 400))
        .take(max_items)
        .collect()
}

#[derive(Debug, Clone)]
struct ShapedPlannerMessage {
    assistant_message: String,
    artifacts: Vec<PlannerArtifact>,
    response_truncated: bool,
    large_artifacts: bool,
}

fn shape_planner_assistant_message(
    raw_message: String,
    readiness: PlannerReadiness,
    plan: Option<&PlanDraft>,
    source_truncated: bool,
) -> ShapedPlannerMessage {
    let (message_without_tables, artifacts) = extract_planner_artifacts_from_markdown(&raw_message);
    let mut response_truncated = source_truncated;
    let mut assistant_message = if readiness == PlannerReadiness::Ready {
        ready_start_work_message(plan)
    } else if let Some(plan) = plan
        .filter(|plan| !plan.open_questions.is_empty())
        .filter(|_| planner_message_claims_start_work_ready(&message_without_tables))
    {
        clarification_summary_message(plan)
    } else if source_truncated {
        match plan {
            Some(plan) if !plan.open_questions.is_empty() => clarification_summary_message(plan),
            _ => crate::PLANNER_TRUNCATED_NOTICE.to_owned(),
        }
    } else {
        message_without_tables.trim().to_owned()
    };

    if !artifacts.is_empty() && !source_truncated && readiness != PlannerReadiness::Ready {
        assistant_message = append_sentence(
            assistant_message,
            "I moved structured details into artifacts so the chat stays readable.",
        );
    }

    assistant_message = sanitize_planner_execution_claims(&assistant_message);
    let max_words = if readiness == PlannerReadiness::Ready {
        crate::PLANNER_READY_WORD_LIMIT
    } else {
        crate::PLANNER_NORMAL_WORD_LIMIT
    };
    let (bounded_message, bounded_truncated) = limit_visible_words(&assistant_message, max_words);
    response_truncated |= bounded_truncated;
    let assistant_message =
        if response_truncated && !bounded_message.contains(crate::PLANNER_TRUNCATED_NOTICE) {
            append_sentence(crate::PLANNER_TRUNCATED_NOTICE.to_owned(), &bounded_message)
        } else {
            bounded_message
        };
    let large_artifacts = artifacts.iter().any(planner_artifact_is_large);

    ShapedPlannerMessage {
        assistant_message,
        artifacts,
        response_truncated,
        large_artifacts,
    }
}

pub(crate) fn live_message_was_length_truncated(message: Option<&LivePlannerMessage>) -> bool {
    message
        .and_then(|message| message.finish_reason.as_deref())
        .map(|reason| {
            let normalized = reason.to_ascii_lowercase();
            normalized == "length" || normalized == "max_tokens"
        })
        .unwrap_or(false)
}

fn ready_start_work_message(plan: Option<&PlanDraft>) -> String {
    let mut parts = vec![
        "I'm ready. Click Start Work and I'll execute this through the native executor.".to_owned(),
    ];
    if let Some(plan) = plan {
        if !plan.goal.trim().is_empty() {
            parts.push(format!("Goal: {}", single_line_preview(&plan.goal, 180)));
        }
        if !plan.acceptance_criteria.is_empty() {
            parts.push(format!(
                "Checks: {}",
                plan.acceptance_criteria
                    .iter()
                    .take(2)
                    .map(|item| single_line_preview(item, 120))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }
    parts.join("\n")
}

fn planner_message_claims_start_work_ready(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    !lower.contains("not ready")
        && (lower.contains("click start work")
            || lower.contains("ready for start work")
            || (lower.contains("ready") && lower.contains("start work")))
}

fn clarification_summary_message(plan: &PlanDraft) -> String {
    format!(
        "Before Start Work can run through the native executor, I need:\n{}",
        numbered_lines(&plan.open_questions)
    )
}

fn append_sentence(mut current: String, sentence: &str) -> String {
    let sentence = sentence.trim();
    if sentence.is_empty() {
        return current;
    }
    if current.trim().is_empty() {
        return sentence.to_owned();
    }
    if !current.ends_with('\n') {
        current.push_str("\n\n");
    }
    current.push_str(sentence);
    current
}

fn sanitize_planner_execution_claims(message: &str) -> String {
    let mut sanitized = message
        .replace("Discuss mode", "Planner Chat")
        .replace("discuss mode", "Planner Chat")
        .replace("Work mode", "Start Work")
        .replace("work mode", "Start Work");
    let lower = sanitized.to_ascii_lowercase();
    let claimed_execution = [
        "i edited",
        "i have edited",
        "i created",
        "i have created",
        "i updated",
        "i have updated",
        "i implemented",
        "i have implemented",
        "i ran ",
        "i have run ",
        "files are changed",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase));
    if claimed_execution {
        sanitized = "I can plan this here, but I do not edit files or run commands in chat. When this is ready, click Start Work and I will execute it through the native executor.".to_owned();
    }
    sanitized
}

fn limit_visible_words(message: &str, max_words: usize) -> (String, bool) {
    let words = message.split_whitespace().collect::<Vec<_>>();
    if words.len() <= max_words {
        return (message.trim().to_owned(), false);
    }
    (words[..max_words].join(" "), true)
}

fn single_line_preview(value: &str, max_chars: usize) -> String {
    let mut preview = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if preview.chars().count() > max_chars {
        preview = preview
            .chars()
            .take(max_chars.saturating_sub(3))
            .collect::<String>()
            .trim_end()
            .to_owned();
        preview.push_str("...");
    }
    preview
}

fn planner_artifact_is_large(artifact: &PlannerArtifact) -> bool {
    match artifact {
        PlannerArtifact::Table { rows, .. } => rows.len() > 8,
        PlannerArtifact::Notes { items, .. } => items.len() > 8,
        PlannerArtifact::Text { content, .. } => content.split_whitespace().count() > 120,
    }
}

fn extract_planner_artifacts_from_markdown(message: &str) -> (String, Vec<PlannerArtifact>) {
    let lines = message.lines().collect::<Vec<_>>();
    let mut kept = Vec::new();
    let mut artifacts = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if index + 1 < lines.len()
            && is_markdown_table_row(lines[index])
            && is_markdown_table_separator(lines[index + 1])
        {
            let columns = split_markdown_table_row(lines[index]);
            let mut rows = Vec::new();
            index += 2;
            while index < lines.len() && is_markdown_table_row(lines[index]) {
                let row = split_markdown_table_row(lines[index]);
                if !row.is_empty() {
                    rows.push(row);
                }
                index += 1;
            }
            if !columns.is_empty() && !rows.is_empty() {
                let collapsed = rows.len() > 6;
                artifacts.push(PlannerArtifact::Table {
                    title: format!("Planner table {}", artifacts.len() + 1),
                    columns,
                    rows,
                    collapsed,
                });
                continue;
            }
        }
        kept.push(lines[index]);
        index += 1;
    }
    (kept.join("\n"), artifacts)
}

fn is_markdown_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.matches('|').count() >= 2
}

fn is_markdown_table_separator(line: &str) -> bool {
    let trimmed = line.trim();
    is_markdown_table_row(trimmed)
        && trimmed
            .chars()
            .all(|ch| matches!(ch, '|' | '-' | ':' | ' '))
}

fn split_markdown_table_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().trim_matches('`').to_owned())
        .filter(|cell| !cell.is_empty())
        .collect()
}

fn deterministic_planner_message(
    mode: &str,
    plan: &PlanDraft,
    readiness: PlannerReadiness,
    confirmed: bool,
) -> String {
    if readiness == PlannerReadiness::NeedsClarification {
        return format!(
            "I can plan this, but I need clarification before Start Work can run:\n{}",
            numbered_lines(&plan.open_questions)
        );
    }
    if mode == "work" && confirmed {
        return format!(
            "The plan is ready for workflow '{}'. Use Start Work to run it and get evidence against the acceptance criteria.",
            plan.selected_workflow_id
        );
    }
    if mode == "work" {
        return format!(
            "The plan is ready for workflow '{}'. Use Start Work when you want me to execute it. Acceptance criteria:\n{}",
            plan.selected_workflow_id,
            numbered_lines(&plan.acceptance_criteria)
        );
    }
    format!(
        "I have enough information to plan this. Goal: {}\nAcceptance criteria:\n{}\nUse Start Work when you want me to execute it.",
        plan.goal,
        numbered_lines(&plan.acceptance_criteria)
    )
}

fn planner_plan_overview_message(plan: &PlanDraft) -> String {
    let scope = if plan.scope.is_empty() {
        "the requested target workspace".to_owned()
    } else {
        plan.scope.join(", ")
    };
    let criteria = if plan.acceptance_criteria.is_empty() {
        "1. The requested work is implemented.\n2. Relevant checks or manual verification are recorded."
            .to_owned()
    } else {
        numbered_lines(&plan.acceptance_criteria)
    };
    let steps = if plan.steps.is_empty() {
        numbered_lines(&[
            "Confirm the scoped goal and constraints.".to_owned(),
            "Inspect or create the target project structure.".to_owned(),
            "Implement the smallest complete version of the requested behavior.".to_owned(),
            "Run practical verification and report evidence, risks, and next steps.".to_owned(),
        ])
    } else {
        numbered_lines(&plan.steps)
    };
    format!(
        "Plan before Start Work:\nGoal: {}\nScope: {}\nSteps:\n{}\nCompletion standard:\n{}\n\nI will not execute this until Start Work.",
        plan.goal, scope, steps, criteria
    )
}

fn planner_plan_draft(request: &PlannerConversationRequest) -> PlanDraft {
    let current = request.current_plan.clone();
    let affected_paths = unique_strings(extract_affected_paths(&request.message));
    let whole_repo_scope = message_has_whole_repo_scope(&request.message);
    let acceptance_criteria = {
        let parsed = unique_strings(extract_acceptance_criteria(&request.message));
        if !parsed.is_empty() {
            parsed
        } else {
            current
                .as_ref()
                .map(|plan| plan.acceptance_criteria.clone())
                .filter(|items| !items.is_empty())
                .unwrap_or_else(|| {
                    vec!["The workflow ends with an evidence-backed final report.".to_owned()]
                })
        }
    };
    let mut open_questions = Vec::new();
    if affected_paths.is_empty()
        && current
            .as_ref()
            .map(|plan| plan.affected_paths.is_empty() && plan.scope.is_empty())
            .unwrap_or(true)
        && !whole_repo_scope
    {
        open_questions
            .push("Which path, module, or repository scope should I focus on?".to_owned());
    }
    if acceptance_criteria.is_empty() {
        open_questions
            .push("Which checks or acceptance criteria should prove completion?".to_owned());
    }
    if request.message.trim().len() < 12 {
        open_questions
            .push("What exact change or investigation should the workflow perform?".to_owned());
    }
    open_questions = unique_strings(open_questions);
    let confirmation_like = current.is_some() && message_confirms_existing_plan(&request.message);
    let goal = if confirmation_like {
        current
            .as_ref()
            .map(|plan| plan.goal.clone())
            .unwrap_or_else(|| "Complete the requested repository work.".to_owned())
    } else {
        extract_goal(&request.message)
            .or_else(|| current.as_ref().map(|plan| plan.goal.clone()))
            .unwrap_or_else(|| "Complete the requested repository work.".to_owned())
    };
    let scope = if affected_paths.is_empty() {
        if whole_repo_scope {
            vec![".".to_owned()]
        } else {
            current
                .as_ref()
                .map(|plan| plan.scope.clone())
                .unwrap_or_default()
        }
    } else {
        affected_paths.clone()
    };
    let risks = {
        let parsed = unique_strings(extract_risks(&request.message));
        if !parsed.is_empty() {
            parsed
        } else {
            current
                .as_ref()
                .map(|plan| plan.risks.clone())
                .filter(|items| !items.is_empty())
                .unwrap_or_else(|| {
                    vec!["Behavior may change if the affected scope is too broad.".to_owned()]
                })
        }
    };
    let memory_proposals = {
        let parsed = memory_proposals_for(&request.message);
        if parsed.is_empty() {
            current
                .as_ref()
                .map(|plan| plan.memory_proposals.clone())
                .unwrap_or_default()
        } else {
            parsed
        }
    };
    PlanDraft {
        goal,
        execution_mode: current
            .as_ref()
            .map(|plan| plan.execution_mode.clone())
            .unwrap_or_else(|| {
                if message_explicitly_read_only(&request.message) {
                    PlanExecutionMode::ReadOnly
                } else {
                    PlanExecutionMode::default()
                }
            }),
        review_mode: current
            .as_ref()
            .map(|plan| plan.review_mode.clone())
            .unwrap_or_else(|| {
                if message_explicitly_qualitative(&request.message) {
                    PlanReviewMode::Qualitative
                } else {
                    PlanReviewMode::default()
                }
            }),
        scope,
        non_goals: current
            .as_ref()
            .map(|plan| plan.non_goals.clone())
            .unwrap_or_else(|| vec!["Do not change unrelated product surfaces.".to_owned()]),
        assumptions: current
            .as_ref()
            .map(|plan| plan.assumptions.clone())
            .unwrap_or_else(|| {
                vec![
                    "Normal validation must stay offline.".to_owned(),
                    "Current repo evidence overrides stale memory.".to_owned(),
                ]
            }),
        steps: if confirmation_like {
            current
                .as_ref()
                .map(|plan| plan.steps.clone())
                .filter(|steps| !steps.is_empty())
                .unwrap_or_else(|| plan_steps_for(&request.message))
        } else {
            plan_steps_for(&request.message)
        },
        affected_paths,
        acceptance_criteria,
        risks,
        open_questions,
        selected_workflow_id: request.workflow_id.clone(),
        memory_proposals,
    }
}

fn memory_proposals_for(message: &str) -> Vec<MemoryProposalDraft> {
    let lower = message.to_ascii_lowercase();
    if !(lower.contains("remember")
        || lower.contains("preference")
        || lower.contains("project convention")
        || lower.contains("\u{8bb0}\u{4f4f}"))
    {
        return Vec::new();
    }
    let content = message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(message.trim())
        .trim_matches(|ch: char| ch == '"' || ch == '\'')
        .to_owned();
    if content.is_empty() {
        return Vec::new();
    }
    vec![MemoryProposalDraft {
        scope: "project".to_owned(),
        key: stable_memory_key(&content),
        content,
        rationale: "The user phrased this as a durable preference or project convention."
            .to_owned(),
        requires_confirmation: true,
    }]
}

fn stable_memory_key(content: &str) -> String {
    let key = content
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch.is_whitespace() || matches!(ch, '-' | '_' | '/' | '.') {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    if key.is_empty() {
        "planner-memory-proposal".to_owned()
    } else {
        key
    }
}

pub(crate) fn numbered_lines(items: &[String]) -> String {
    items
        .iter()
        .enumerate()
        .map(|(index, item)| format!("{}. {}", index + 1, item))
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_looks_like_work(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let work_markers = [
        "add",
        "build",
        "change",
        "check",
        "code",
        "delete",
        "fix",
        "implement",
        "inspect",
        "plan",
        "patch",
        "refactor",
        "repo",
        "run",
        "test",
        "update",
        "work",
        "workflow",
    ];
    work_markers.iter().any(|marker| lower.contains(marker))
        || !extract_affected_paths(message).is_empty()
}

fn message_requests_planning_only(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "do not execute yet",
        "don't execute yet",
        "dont execute yet",
        "do not run yet",
        "don't run yet",
        "dont run yet",
        "do not start work",
        "don't start work",
        "dont start work",
        "before start work",
        "first give me",
        "give me your plan",
        "concrete plan",
        "plan first",
        "explain your plan",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || message.contains("\u{5148}\u{4e0d}\u{8981}\u{6267}\u{884c}")
        || message.contains("\u{6682}\u{4e0d}\u{6267}\u{884c}")
        || message.contains("\u{4e0d}\u{8981}\u{6267}\u{884c}")
        || message.contains("\u{5148}\u{544a}\u{8bc9}\u{6211}\u{4f60}\u{7684}\u{8ba1}\u{5212}")
        || message.contains("\u{5148}\u{7ed9}\u{8ba1}\u{5212}")
        || message.contains("\u{7ed9}\u{6211}\u{8ba1}\u{5212}")
        || message.contains("\u{89c4}\u{5212}")
}

fn planner_message_lacks_plan_details(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let has_start_work_prompt = lower.contains("click start work") || lower.contains("start work");
    let detail_markers = [
        "plan",
        "steps",
        "approach",
        "file",
        "layout",
        "gameplay",
        "verify",
        "validation",
        "completion",
    ];
    let detail_count = detail_markers
        .iter()
        .filter(|marker| lower.contains(**marker))
        .count();
    has_start_work_prompt && detail_count < 2
}

fn message_confirms_existing_plan(message: &str) -> bool {
    let trimmed = message.trim();
    let lower = trimmed.to_ascii_lowercase();
    let confirmation_markers = [
        "proceed",
        "go ahead",
        "confirm",
        "confirmed",
        "ready",
        "start work",
        "execute it",
        "execute this",
        "run it",
        "your own plan",
        "looks good",
        "keep it simple",
    ];
    let short_confirmation = matches!(
        lower.as_str(),
        "yes" | "y" | "ok" | "okay" | "sure" | "approved" | "ship it"
    );
    short_confirmation
        || confirmation_markers
            .iter()
            .any(|marker| lower.contains(marker))
        || trimmed.contains("\u{53ef}\u{4ee5}")
        || trimmed.contains("\u{7ee7}\u{7eed}")
        || trimmed.contains("\u{786e}\u{8ba4}")
        || trimmed.contains("\u{5f00}\u{59cb}")
        || trimmed.contains("\u{6267}\u{884c}")
}

pub(crate) fn message_is_pure_plan_confirmation(message: &str) -> bool {
    let mut normalized = String::new();
    for ch in message.trim().chars() {
        if ch.is_alphanumeric() {
            normalized.extend(ch.to_lowercase());
        } else if !normalized.ends_with(' ') {
            normalized.push(' ');
        }
    }
    let normalized = normalized.trim();
    matches!(
        normalized,
        "yes"
            | "y"
            | "ok"
            | "okay"
            | "sure"
            | "approved"
            | "proceed"
            | "go ahead"
            | "looks good"
            | "the plan looks good"
            | "keep it simple"
            | "the plan looks good keep it simple"
            | "start work"
            | "ship it"
            | "\u{53ef}\u{4ee5}"
            | "\u{7ee7}\u{7eed}"
            | "\u{786e}\u{8ba4}"
            | "\u{5f00}\u{59cb}"
            | "\u{6267}\u{884c}"
    ) || (normalized.starts_with("confirm ")
        && normalized.ends_with(" plan")
        && normalized.split_whitespace().count() <= 8)
}

fn message_explicitly_read_only(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("read-only")
        || lower.contains("read only")
        || lower.contains("without modifying")
        || lower.contains("without changing files")
        || lower.contains("do not modify")
        || message.contains("\u{53ea}\u{8bfb}")
        || message.contains("\u{4e0d}\u{4fee}\u{6539}")
}

fn message_explicitly_qualitative(message: &str) -> bool {
    message.to_ascii_lowercase().contains("qualitative")
        || message.contains("\u{5b9a}\u{6027}\u{8bc4}\u{5ba1}")
        || message.contains("\u{5b9a}\u{6027}\u{8bc4}\u{4f30}")
}

fn message_has_whole_repo_scope(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("whole repo")
        || lower.contains("entire repo")
        || lower.contains("this repo")
        || lower.contains("this repository")
        || lower.contains("empty repo")
        || lower.contains("empty repository")
        || lower.contains("project")
        || lower.contains("repo root")
        || lower.contains("repository root")
        || lower.contains("current repo")
        || lower.contains("current repository")
        || lower.contains("workspace root")
        || message.contains("\u{5f53}\u{524d}\u{4ed3}\u{5e93}")
        || message.contains("\u{4ed3}\u{5e93}\u{6839}\u{76ee}\u{5f55}")
        || message.contains("\u{9879}\u{76ee}\u{6839}\u{76ee}\u{5f55}")
}

fn extract_goal(message: &str) -> Option<String> {
    message
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| {
            line.trim_matches(|ch: char| ch == '"' || ch == '\'')
                .to_owned()
        })
        .filter(|line| !line.is_empty())
}

fn extract_affected_paths(message: &str) -> Vec<String> {
    message
        .split_whitespace()
        .filter_map(|token| {
            let cleaned = token
                .trim_matches(|ch: char| {
                    matches!(
                        ch,
                        ',' | ';'
                            | ':'
                            | ')'
                            | '('
                            | '['
                            | ']'
                            | '"'
                            | '\''
                            | '`'
                            | '\u{ff0c}'
                            | '\u{3002}'
                    )
                })
                .replace('\\', "/");
            let lower = cleaned.to_ascii_lowercase();
            let path_like = cleaned.contains('/')
                || [
                    ".rs", ".tsx", ".ts", ".js", ".jsx", ".md", ".toml", ".yaml", ".yml", ".json",
                    ".css", ".ps1", ".sh",
                ]
                .iter()
                .any(|suffix| lower.ends_with(suffix));
            if path_like && !cleaned.contains("://") {
                Some(cleaned)
            } else {
                None
            }
        })
        .collect()
}

fn extract_acceptance_criteria(message: &str) -> Vec<String> {
    let mut criteria = Vec::new();
    for line in message.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        let markers = [
            ("acceptance:", true),
            ("success criteria:", true),
            ("\u{9a8c}\u{6536}:", false),
            ("\u{9a8c}\u{6536}\u{ff1a}", false),
            ("\u{6210}\u{529f}\u{6807}\u{51c6}:", false),
            ("\u{6210}\u{529f}\u{6807}\u{51c6}\u{ff1a}", false),
            ("\u{5b8c}\u{6210}\u{6807}\u{51c6}:", false),
            ("\u{5b8c}\u{6210}\u{6807}\u{51c6}\u{ff1a}", false),
        ];
        if let Some(value) = markers.iter().find_map(|(marker, ascii_case_fold)| {
            let index = if *ascii_case_fold {
                lower.find(marker)
            } else {
                line.find(marker)
            }?;
            line.get(index + marker.len()..).map(str::trim)
        }) {
            if !value.is_empty() {
                criteria.extend(split_list_like(value));
            }
        }
    }
    let lower = message.to_ascii_lowercase();
    if lower.contains("test") {
        criteria.push("Relevant tests pass.".to_owned());
    }
    if lower.contains("build") {
        criteria.push("The build passes.".to_owned());
    }
    criteria
}

fn extract_risks(message: &str) -> Vec<String> {
    let mut risks = Vec::new();
    for line in message.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        let markers = [
            ("risk:", true),
            ("risks:", true),
            ("\u{98ce}\u{9669}:", false),
            ("\u{98ce}\u{9669}\u{ff1a}", false),
        ];
        if let Some(value) = markers.iter().find_map(|(marker, ascii_case_fold)| {
            let index = if *ascii_case_fold {
                lower.find(marker)
            } else {
                line.find(marker)
            }?;
            line.get(index + marker.len()..).map(str::trim)
        }) {
            if !value.is_empty() {
                risks.extend(split_list_like(value));
            }
        }
    }
    risks
}

fn split_list_like(value: &str) -> Vec<String> {
    value
        .split([';', '|', '\u{ff1b}', '\u{3001}'])
        .map(|item| item.trim().trim_start_matches('-').trim())
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn plan_steps_for(message: &str) -> Vec<String> {
    let mut steps = vec![
        "Confirm the scoped goal and acceptance criteria.".to_owned(),
        "Gather bounded repository evidence for the affected scope.".to_owned(),
        "Execute the selected workflow through role-specific harnesses.".to_owned(),
        "Report checks, evidence, patches, blockers, and next steps.".to_owned(),
    ];
    if message.to_ascii_lowercase().contains("refactor") {
        steps.insert(2, "Preserve behavior while changing structure.".to_owned());
    }
    steps
}

fn unique_strings(items: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    items
        .into_iter()
        .filter_map(|item| {
            let item = item.trim().to_owned();
            if item.is_empty() || !seen.insert(item.clone()) {
                None
            } else {
                Some(item)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        extract_acceptance_criteria, extract_affected_paths, extract_risks, merge_model_plan,
        message_explicitly_qualitative, message_explicitly_read_only, message_has_whole_repo_scope,
        message_is_pure_plan_confirmation, parse_model_planner_envelope,
        planner_message_claims_start_work_ready,
    };
    use crate::api_types::{PlanDraft, PlanExecutionMode, PlanReviewMode};

    #[test]
    fn whole_repo_scope_detects_repository_root_phrases() {
        assert!(message_has_whole_repo_scope(
            "Use the current repository root."
        ));
        assert!(message_has_whole_repo_scope(
            "Please work from the repo root."
        ));
        assert!(message_has_whole_repo_scope("The workspace root is fine."));
    }

    #[test]
    fn ready_claim_detector_ignores_negative_ready_statements() {
        assert!(planner_message_claims_start_work_ready(
            "I'm ready. Click Start Work."
        ));
        assert!(!planner_message_claims_start_work_ready(
            "I am not ready for Start Work yet."
        ));
    }

    #[test]
    fn qualified_confirmation_without_new_scope_is_pure_confirmation() {
        assert!(message_is_pure_plan_confirmation(
            "Confirm this read-only qualitative review plan."
        ));
        assert!(!message_is_pure_plan_confirmation(
            "Confirm this plan and also edit README.md."
        ));
    }

    #[test]
    fn explicit_typed_constraints_are_detected_without_generic_task_classification() {
        assert!(message_explicitly_read_only(
            "Review README.md without modifying files."
        ));
        assert!(message_explicitly_read_only(
            "\u{53ea}\u{8bfb}\u{8bc4}\u{5ba1} README.md"
        ));
        assert!(message_explicitly_qualitative(
            "Perform a qualitative review."
        ));
        assert!(!message_explicitly_read_only(
            "Review and improve README.md."
        ));
    }

    #[test]
    fn model_plan_adds_domain_quality_without_changing_workflow_authority() {
        let envelope = parse_model_planner_envelope(
            r#"{
                "assistant_message":"The plan includes the expected game loop.",
                "ready_for_start_work":true,
                "plan_draft":{
                    "goal":["Build a recognizable lane-defense browser game."],
                    "execution_mode":"must_write",
                    "review_mode":"qualitative",
                    "steps":["Implement resource, placement, wave, combat, win, and loss loops."],
                    "acceptance_criteria":["At least two strategically distinct plant types are usable.","A complete wave can be won or lost."],
                    "risks":["Visual polish must not replace gameplay depth."],
                    "selected_workflow_id":"untrusted-model-choice"
                }
            }"#,
        )
        .expect("structured planner envelope");
        let deterministic = PlanDraft {
            goal: "Make a plant game.".to_owned(),
            execution_mode: PlanExecutionMode::ReadOnly,
            review_mode: PlanReviewMode::Standard,
            scope: vec![".".to_owned()],
            non_goals: Vec::new(),
            assumptions: Vec::new(),
            steps: vec!["Implement the task.".to_owned()],
            affected_paths: Vec::new(),
            acceptance_criteria: vec!["The workflow completes.".to_owned()],
            risks: Vec::new(),
            open_questions: vec!["Which output path should be used?".to_owned()],
            selected_workflow_id: "planner-led".to_owned(),
            memory_proposals: Vec::new(),
        };

        let merged = merge_model_plan(
            deterministic,
            envelope.plan_draft.unwrap(),
            &[],
            &[],
            &[],
            false,
            false,
        );

        assert!(merged.acceptance_criteria[0].contains("plant types"));
        assert!(merged.steps[0].contains("resource"));
        assert_eq!(merged.selected_workflow_id, "planner-led");
        assert!(merged.open_questions.is_empty());
        assert!(matches!(
            merged.execution_mode,
            PlanExecutionMode::MustWrite
        ));
        assert!(matches!(merged.review_mode, PlanReviewMode::Qualitative));
    }

    #[test]
    fn confirmation_preserves_existing_typed_control_modes() {
        let deterministic = PlanDraft {
            goal: "Review README.md.".to_owned(),
            execution_mode: PlanExecutionMode::ReadOnly,
            review_mode: PlanReviewMode::Qualitative,
            scope: vec!["README.md".to_owned()],
            non_goals: Vec::new(),
            assumptions: Vec::new(),
            steps: Vec::new(),
            affected_paths: vec!["README.md".to_owned()],
            acceptance_criteria: vec!["Findings cite current content.".to_owned()],
            risks: Vec::new(),
            open_questions: Vec::new(),
            selected_workflow_id: "planner-led".to_owned(),
            memory_proposals: Vec::new(),
        };
        let mut model = deterministic.clone();
        model.execution_mode = PlanExecutionMode::MustWrite;
        model.review_mode = PlanReviewMode::Standard;

        let merged = merge_model_plan(deterministic, model, &[], &[], &[], true, true);

        assert!(matches!(merged.execution_mode, PlanExecutionMode::ReadOnly));
        assert!(matches!(merged.review_mode, PlanReviewMode::Qualitative));
    }

    #[test]
    fn model_plan_preserves_explicit_multilingual_acceptance_criteria() {
        let explicit = extract_acceptance_criteria(
            "Acceptance: keyboard navigation works; restart resets the game\n\u{9a8c}\u{6536}\u{ff1a}\u{89e6}\u{6478}\u{64cd}\u{4f5c}\u{53ef}\u{4ee5}\u{5b8c}\u{6210}\u{4e00}\u{5c40}\u{ff1b}\u{754c}\u{9762}\u{4e0d}\u{6ea2}\u{51fa}",
        );
        let deterministic = PlanDraft {
            goal: "Build the game.".to_owned(),
            execution_mode: PlanExecutionMode::MustWrite,
            review_mode: PlanReviewMode::Qualitative,
            scope: vec![".".to_owned()],
            non_goals: Vec::new(),
            assumptions: Vec::new(),
            steps: vec!["Implement the game.".to_owned()],
            affected_paths: Vec::new(),
            acceptance_criteria: explicit.clone(),
            risks: Vec::new(),
            open_questions: Vec::new(),
            selected_workflow_id: "planner-led".to_owned(),
            memory_proposals: Vec::new(),
        };
        let model = PlanDraft {
            acceptance_criteria: vec!["A full wave can be won or lost.".to_owned()],
            ..deterministic.clone()
        };

        let merged = merge_model_plan(deterministic, model, &[], &explicit, &[], false, false);

        assert_eq!(merged.acceptance_criteria.len(), 5);
        assert_eq!(merged.acceptance_criteria[0], "keyboard navigation works");
        assert!(merged.acceptance_criteria.iter().any(|criterion| criterion
            == "\u{89e6}\u{6478}\u{64cd}\u{4f5c}\u{53ef}\u{4ee5}\u{5b8c}\u{6210}\u{4e00}\u{5c40}"));
        assert!(merged
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion == "A full wave can be won or lost."));
    }

    #[test]
    fn model_plan_preserves_current_user_paths_and_risks() {
        let message = "Refactor `crates/coder-server/src/planner_api.rs`\n\u{98ce}\u{9669}\u{ff1a}preserve the public API";
        let explicit_paths = extract_affected_paths(message);
        let explicit_risks = extract_risks(message);
        let deterministic = PlanDraft {
            goal: "Refactor the requested module.".to_owned(),
            execution_mode: PlanExecutionMode::MustWrite,
            review_mode: PlanReviewMode::Standard,
            scope: explicit_paths.clone(),
            non_goals: Vec::new(),
            assumptions: Vec::new(),
            steps: vec!["Implement the scoped change.".to_owned()],
            affected_paths: explicit_paths.clone(),
            acceptance_criteria: vec!["Relevant tests pass.".to_owned()],
            risks: explicit_risks.clone(),
            open_questions: Vec::new(),
            selected_workflow_id: "planner-led".to_owned(),
            memory_proposals: Vec::new(),
        };
        let model = PlanDraft {
            scope: vec!["frontend/src/App.tsx".to_owned()],
            affected_paths: vec!["frontend/src/App.tsx".to_owned()],
            risks: vec!["The UI may need regression testing.".to_owned()],
            ..deterministic.clone()
        };

        let merged = merge_model_plan(
            deterministic,
            model,
            &explicit_paths,
            &[],
            &explicit_risks,
            false,
            false,
        );

        assert_eq!(merged.scope[0], "crates/coder-server/src/planner_api.rs");
        assert_eq!(
            merged.affected_paths[0],
            "crates/coder-server/src/planner_api.rs"
        );
        assert_eq!(merged.risks[0], "preserve the public API");
        assert!(merged
            .risks
            .iter()
            .any(|risk| risk == "The UI may need regression testing."));
    }

    #[test]
    fn pure_confirmation_does_not_consume_supplementary_requirements() {
        assert!(message_is_pure_plan_confirmation(
            "The plan looks good. Keep it simple."
        ));
        assert!(message_is_pure_plan_confirmation("\u{53ef}\u{4ee5}"));
        assert!(!message_is_pure_plan_confirmation(
            "Looks good, but also add keyboard controls."
        ));
        assert!(!message_is_pure_plan_confirmation(
            "Confirm the plan and create README.md only."
        ));
    }
}
