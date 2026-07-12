use coder_core::{FinalReport, RunId};
use coder_events::CoderEvent;
use serde_json::Value;

use crate::api_types::{
    ApprovalItem, CommandExecutionItem, ExecutorStepItem, FileChangeItem, FinalSummaryItem,
    MessageTimelineItem, PlanUpdateItem, ReasoningSummaryItem, TimelineItem, ToolCallItem,
    VerificationItem,
};
use crate::{
    changed_files_from_payload, command_from_payload, payload_i64, payload_string, payload_u64,
    public_preview, report_status_string,
};

pub(crate) fn project_timeline_items(
    run_id: &RunId,
    events: &[CoderEvent],
    report: Option<&FinalReport>,
) -> Vec<TimelineItem> {
    let mut items = Vec::new();
    for event in events {
        let created_at = event.timestamp.to_string();
        match event.kind.as_str() {
            "run.started" => {
                let task = payload_string(&event.payload, "task")
                    .unwrap_or_else(|| "Workflow started.".to_owned());
                items.push(TimelineItem::PlanUpdate(PlanUpdateItem {
                    id: timeline_id(event, "plan"),
                    agent_id: "planner".to_owned(),
                    title: "Work started".to_owned(),
                    summary: public_preview(&task, 800),
                    created_at,
                }));
            }
            "planner.message.completed" => {
                let summary = payload_string(&event.payload, "summary")
                    .or_else(|| payload_string(&event.payload, "message"))
                    .unwrap_or_else(|| "Planner updated the run.".to_owned());
                items.push(TimelineItem::PlannerMessage(MessageTimelineItem {
                    id: timeline_id(event, "planner-message"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "planner".to_owned()),
                    content: public_preview(&summary, 800),
                    created_at,
                }));
            }
            "planner.plan.updated" => {
                items.push(TimelineItem::PlanUpdate(PlanUpdateItem {
                    id: timeline_id(event, "plan-update"),
                    agent_id: "planner".to_owned(),
                    title: "Plan updated".to_owned(),
                    summary: event
                        .payload
                        .get("acceptance_criteria")
                        .and_then(|value| value.as_array())
                        .map(|items| format!("{} acceptance criteria", items.len()))
                        .unwrap_or_else(|| "Planner updated execution context.".to_owned()),
                    created_at,
                }));
            }
            "planner.readiness.changed" | "reasoning.summary" => {
                let summary = payload_string(&event.payload, "summary")
                    .or_else(|| payload_string(&event.payload, "readiness"))
                    .unwrap_or_else(|| "Planner checked readiness.".to_owned());
                items.push(TimelineItem::ReasoningSummary(ReasoningSummaryItem {
                    id: timeline_id(event, "reasoning"),
                    agent_id: "planner".to_owned(),
                    summary_text: vec![public_preview(&summary, 500)],
                    created_at,
                }));
            }
            "executor.reasoning_summary" => {
                let summary = payload_string(&event.payload, "summary")
                    .unwrap_or_else(|| "Executor summarized its next step.".to_owned());
                items.push(TimelineItem::ReasoningSummary(ReasoningSummaryItem {
                    id: timeline_id(event, "reasoning"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    summary_text: vec![public_preview(&summary, 500)],
                    created_at,
                }));
            }
            "executor.action_selected"
            | "executor.next_step"
            | "executor.completed"
            | "executor.blocked"
            | "executor.failed" => {
                let title = match event.kind.as_str() {
                    "executor.action_selected" => "Action selected",
                    "executor.next_step" => "Next step",
                    "executor.completed" => "Executor completed",
                    "executor.blocked" => "Executor blocked",
                    "executor.failed" => "Executor failed",
                    _ => "Executor update",
                };
                items.push(TimelineItem::ExecutorStep(ExecutorStepItem {
                    id: timeline_id(event, "executor"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    title: title.to_owned(),
                    status: timeline_status(event),
                    summary: executor_event_summary(&event.payload)
                        .map(|value| public_preview(&value, 500)),
                    created_at,
                }));
            }
            "backend.selected" | "backend.blocked" => {
                items.push(TimelineItem::ExecutorStep(ExecutorStepItem {
                    id: timeline_id(event, "backend"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    title: backend_timeline_title(&event.kind, &event.payload),
                    status: timeline_status(event),
                    summary: payload_string(&event.payload, "summary")
                        .or_else(|| payload_string(&event.payload, "reason"))
                        .map(|value| public_preview(&value, 500)),
                    created_at,
                }));
            }
            "observation.recorded" => {
                let summary = payload_string(&event.payload, "summary")
                    .unwrap_or_else(|| "Observation recorded.".to_owned());
                items.push(TimelineItem::ExecutorStep(ExecutorStepItem {
                    id: timeline_id(event, "observation"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    title: "Observation recorded".to_owned(),
                    status: timeline_status(event),
                    summary: Some(public_preview(&summary, 500)),
                    created_at,
                }));
            }
            "model.provider_turn.completed" => {
                let turn = payload_u64(&event.payload, "turn").unwrap_or(0);
                let estimated_input =
                    payload_u64(&event.payload, "estimated_input_tokens").unwrap_or(0);
                let input = payload_u64(&event.payload, "input_tokens");
                let output = payload_u64(&event.payload, "output_tokens");
                let cache_read = payload_u64(&event.payload, "cache_read_tokens");
                let summary = if let (Some(input), Some(output)) = (input, output) {
                    format!(
                        "Input {input} tokens, output {output} tokens, cache read {} tokens.",
                        cache_read.unwrap_or(0)
                    )
                } else {
                    format!("Estimated input {estimated_input} tokens; provider usage unavailable.")
                };
                items.push(TimelineItem::ExecutorStep(ExecutorStepItem {
                    id: timeline_id(event, "model-turn"),
                    agent_id: "executor".to_owned(),
                    title: format!("Model turn {turn}"),
                    status: "done".to_owned(),
                    summary: Some(summary),
                    created_at,
                }));
            }
            "node.started" | "node.completed" | "agent.called" | "agent.completed"
            | "workflow.started" | "round.started" | "run.completed" | "run.failed"
            | "run.blocked" | "run.cancelled" => {
                items.push(TimelineItem::ExecutorStep(ExecutorStepItem {
                    id: timeline_id(event, "step"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .or_else(|| payload_string(&event.payload, "node_id"))
                        .unwrap_or_else(|| "executor".to_owned()),
                    title: event.kind.replace('.', " "),
                    status: status_from_event_kind(&event.kind),
                    summary: payload_string(&event.payload, "summary")
                        .or_else(|| payload_string(&event.payload, "message"))
                        .map(|value| public_preview(&value, 500)),
                    created_at,
                }));
            }
            "tool.started" | "tool.completed" | "tool.failed" | "tool.called" | "tool.result"
            | "mcp.tool.called" | "mcp.tool.completed" => {
                items.push(TimelineItem::ToolCall(ToolCallItem {
                    id: timeline_id(event, "tool"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    tool_name: payload_string(&event.payload, "tool_name")
                        .or_else(|| payload_string(&event.payload, "tool"))
                        .or_else(|| payload_string(&event.payload, "node_id"))
                        .unwrap_or_else(|| "tool".to_owned()),
                    status: timeline_status(event),
                    summary: payload_string(&event.payload, "result_summary")
                        .or_else(|| payload_string(&event.payload, "summary"))
                        .map(|value| public_preview(&value, 500)),
                    evidence_ref: first_event_ref(event),
                    created_at,
                }));
            }
            "command.previewed" | "command.completed" | "command.failed" => {
                items.push(TimelineItem::CommandExecution(CommandExecutionItem {
                    id: timeline_id(event, "command"),
                    agent_id: "executor".to_owned(),
                    command: command_from_payload(&event.payload),
                    cwd: payload_string(&event.payload, "cwd").unwrap_or_else(|| ".".to_owned()),
                    status: status_from_event_kind(&event.kind),
                    stdout_preview: payload_string(&event.payload, "stdout_preview")
                        .or_else(|| payload_string(&event.payload, "output"))
                        .map(|value| public_preview(&value, 1000)),
                    stderr_preview: payload_string(&event.payload, "stderr_preview")
                        .map(|value| public_preview(&value, 1000)),
                    exit_code: payload_i64(&event.payload, "returncode")
                        .or_else(|| payload_i64(&event.payload, "exit_code")),
                    duration_ms: payload_u64(&event.payload, "duration_ms"),
                    evidence_ref: first_event_ref(event),
                    created_at,
                }));
            }
            "patch.previewed" | "patch.applied" | "patch.failed" => {
                let files = changed_files_from_payload(&event.payload);
                if files.is_empty() {
                    items.push(TimelineItem::FileChange(FileChangeItem {
                        id: timeline_id(event, "file"),
                        agent_id: "executor".to_owned(),
                        path: payload_string(&event.payload, "patch_file")
                            .unwrap_or_else(|| "patch".to_owned()),
                        change_type: status_from_event_kind(&event.kind),
                        diff_ref: first_event_ref(event),
                        created_at,
                    }));
                } else {
                    for (index, file) in files.into_iter().enumerate() {
                        items.push(TimelineItem::FileChange(FileChangeItem {
                            id: format!("{}-{index}", timeline_id(event, "file")),
                            agent_id: "executor".to_owned(),
                            path: file.path,
                            change_type: file.change_type,
                            diff_ref: first_event_ref(event),
                            created_at: created_at.clone(),
                        }));
                    }
                }
            }
            "approval.requested" | "approval.required" | "approval.recorded" => {
                items.push(TimelineItem::Approval(ApprovalItem {
                    id: timeline_id(event, "approval"),
                    agent_id: payload_string(&event.payload, "agent_id")
                        .unwrap_or_else(|| "executor".to_owned()),
                    risk_level: payload_string(&event.payload, "risk_level")
                        .or_else(|| payload_string(&event.payload, "risk"))
                        .unwrap_or_else(|| "medium".to_owned()),
                    action_type: payload_string(&event.payload, "action_type")
                        .or_else(|| payload_string(&event.payload, "approval_type"))
                        .unwrap_or_else(|| "approval".to_owned()),
                    summary: payload_string(&event.payload, "summary")
                        .or_else(|| payload_string(&event.payload, "reason"))
                        .unwrap_or_else(|| "Approval requested.".to_owned()),
                    status: status_from_event_kind(&event.kind),
                    created_at,
                }));
            }
            "verification.started" | "verification.completed" | "verification.failed" => {
                items.push(TimelineItem::Verification(VerificationItem {
                    id: timeline_id(event, "verification"),
                    agent_id: "executor".to_owned(),
                    status: status_from_event_kind(&event.kind),
                    summary: payload_string(&event.payload, "summary")
                        .or_else(|| payload_string(&event.payload, "command"))
                        .unwrap_or_else(|| "Verification step.".to_owned()),
                    evidence_ref: first_event_ref(event),
                    created_at,
                }));
            }
            _ => {}
        }
    }
    if let Some(report) = report {
        items.push(TimelineItem::FinalSummary(FinalSummaryItem {
            id: format!("timeline-final-{}", run_id.as_str()),
            agent_id: "planner".to_owned(),
            status: report_status_string(report.status),
            summary: public_preview(&report.summary, 1200),
            changed_files: report.changed_files.clone(),
            checks: report.checks.clone(),
            evidence_refs: report.evidence_refs.clone(),
            blockers: report.blockers.clone(),
            next_steps: report.next_steps.clone(),
            created_at: events
                .last()
                .map(|event| event.timestamp.to_string())
                .unwrap_or_default(),
        }));
    }
    items
}

fn timeline_id(event: &CoderEvent, suffix: &str) -> String {
    format!("{}-{suffix}", event.event_id)
}

fn first_event_ref(event: &CoderEvent) -> Option<String> {
    event.refs.first().map(|reference| reference.uri.clone())
}

fn timeline_status(event: &CoderEvent) -> String {
    payload_string(&event.payload, "status").unwrap_or_else(|| status_from_event_kind(&event.kind))
}

fn executor_event_summary(payload: &Value) -> Option<String> {
    payload_string(payload, "summary")
        .or_else(|| {
            let tool_name = payload_string(payload, "tool_name")?;
            Some(format!("Selected {tool_name}."))
        })
        .or_else(|| payload_string(payload, "based_on_observation"))
}

fn backend_timeline_title(kind: &str, payload: &Value) -> String {
    let backend = payload_string(payload, "backend").unwrap_or_else(|| "backend".to_owned());
    if kind == "backend.blocked" && payload_string(payload, "fallback_for").is_some() {
        return "Executor backend: blocked".to_owned();
    }
    if backend == "native-rust" && payload_string(payload, "fallback_for").is_some() {
        return "Executor backend: native fallback".to_owned();
    }
    format!(
        "Executor backend: {}",
        timeline_backend_display_name(&backend)
    )
}

fn timeline_backend_display_name(backend: &str) -> &'static str {
    match backend {
        "native-rust" | "native_mock" | "mock" => "Native",
        "planner-model" => "Planner",
        _ => "unknown",
    }
}

fn status_from_event_kind(kind: &str) -> String {
    if kind.ends_with(".failed") || kind == "run.failed" {
        "failed".to_owned()
    } else if kind.ends_with(".blocked") || kind == "run.blocked" {
        "blocked".to_owned()
    } else if kind.ends_with(".completed") || kind == "run.completed" {
        "completed".to_owned()
    } else if kind.ends_with(".started") {
        "running".to_owned()
    } else if kind.ends_with(".requested") || kind.ends_with(".required") {
        "pending".to_owned()
    } else if kind.ends_with(".applied") {
        "applied".to_owned()
    } else if kind.ends_with(".previewed") {
        "previewed".to_owned()
    } else {
        "noted".to_owned()
    }
}
