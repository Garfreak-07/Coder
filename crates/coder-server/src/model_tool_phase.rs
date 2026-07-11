use coder_core::RunId;
use coder_store::RunStore;
use serde_json::{json, Value};

use crate::model_tool_hook_runtime::append_model_tool_event;
use crate::model_tool_permissions::required_permission_for_model_tool;

const MODEL_TOOL_PHASE_CONTRACT: &str = "coder.model_tool_phase.v1";
const MODEL_TOOL_PHASE_PROGRESS_CONTRACT: &str = "coder.model_tool_phase_progress.v1";
const CLAUDE_HOOK_TIMING_DISPLAY_THRESHOLD_MS: u64 = 500;
const CLAUDE_SLOW_PHASE_LOG_THRESHOLD_MS: u64 = 2_000;

pub(crate) struct ModelToolPhaseRecorder {
    store: RunStore,
    run_id: Option<RunId>,
    tool_use_id: String,
    tool_name: String,
    canonical_tool_name: &'static str,
    required_permission_override: Option<String>,
    phases: Vec<Value>,
}

impl ModelToolPhaseRecorder {
    pub(crate) fn new(
        store: &RunStore,
        tool_use_id: &str,
        tool_name: &str,
        canonical_tool_name: &'static str,
        input: &Value,
    ) -> Self {
        Self {
            store: store.clone(),
            run_id: input
                .get("run_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|run_id| !run_id.is_empty())
                .map(|run_id| RunId::from_string(run_id.to_owned())),
            tool_use_id: tool_use_id.to_owned(),
            tool_name: tool_name.to_owned(),
            canonical_tool_name,
            required_permission_override: None,
            phases: Vec::new(),
        }
    }

    pub(crate) fn set_required_permission_override(&mut self, required_permission: Option<&str>) {
        self.required_permission_override = required_permission.map(str::to_owned);
    }

    fn required_permission(&self) -> Option<&str> {
        self.required_permission_override
            .as_deref()
            .or_else(|| required_permission_for_model_tool(self.canonical_tool_name))
    }

    pub(crate) fn record_phase(
        &mut self,
        phase: &str,
        status: &str,
        duration_ms: u64,
        extra: Value,
    ) {
        let mut payload = json!({
            "contract": MODEL_TOOL_PHASE_CONTRACT,
            "source": "coder-server",
            "phase": phase,
            "status": status,
            "tool_use_id": self.tool_use_id,
            "tool_name": self.tool_name,
            "canonical_tool_name": self.canonical_tool_name,
            "required_permission": self.required_permission(),
            "duration_ms": duration_ms,
            "slow_phase_threshold_ms": CLAUDE_SLOW_PHASE_LOG_THRESHOLD_MS,
            "slow_phase": model_tool_slow_phase_observed(phase, duration_ms),
            "claude_sources": claude_tool_phase_sources()
        });
        if model_tool_hook_timing_phase(phase) {
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "hook_timing_display_threshold_ms".to_owned(),
                    json!(CLAUDE_HOOK_TIMING_DISPLAY_THRESHOLD_MS),
                );
                object.insert(
                    "show_inline_timing_summary".to_owned(),
                    json!(duration_ms > CLAUDE_HOOK_TIMING_DISPLAY_THRESHOLD_MS),
                );
            }
        }
        merge_object(&mut payload, extra);
        self.record_phase_event("model_tool.phase", &payload);
        self.phases.push(payload);
    }

    pub(crate) fn record_phase_started(&self, phase: &str) {
        let payload = json!({
            "contract": MODEL_TOOL_PHASE_PROGRESS_CONTRACT,
            "source": "coder-server",
            "progress_kind": "phase_started",
            "phase": phase,
            "status": "started",
            "tool_use_id": self.tool_use_id,
            "tool_name": self.tool_name,
            "canonical_tool_name": self.canonical_tool_name,
            "required_permission": self.required_permission(),
            "claude_sources": claude_tool_phase_progress_sources()
        });
        self.record_phase_event("model_tool.phase.progress", &payload);
    }

    fn record_phase_event(&self, event_kind: &str, payload: &Value) {
        let Some(run_id) = &self.run_id else {
            return;
        };
        let _ = append_model_tool_event(&self.store, run_id, event_kind, payload.clone());
    }

    pub(crate) fn into_phases(self) -> Vec<Value> {
        self.phases
    }
}

fn model_tool_slow_phase_observed(phase: &str, duration_ms: u64) -> bool {
    matches!(
        phase,
        "pre_tool_use_hooks" | "permission_decision" | "post_tool_use_hooks"
    ) && duration_ms >= CLAUDE_SLOW_PHASE_LOG_THRESHOLD_MS
}

fn model_tool_hook_timing_phase(phase: &str) -> bool {
    matches!(phase, "pre_tool_use_hooks" | "post_tool_use_hooks")
}

fn merge_object(target: &mut Value, extra: Value) {
    let (Some(target), Some(extra)) = (target.as_object_mut(), extra.as_object()) else {
        return;
    };
    for (key, value) in extra {
        target.insert(key.clone(), value.clone());
    }
}

fn claude_tool_phase_sources() -> Vec<&'static str> {
    vec![
        "src/services/tools/toolExecution.ts",
        "src/services/tools/StreamingToolExecutor.ts",
        "packages/builtin-tools/src/tools/BashTool/prompt.ts",
        "packages/builtin-tools/src/tools/AgentTool/AgentTool.tsx",
    ]
}

fn claude_tool_phase_progress_sources() -> Vec<&'static str> {
    vec![
        "src/services/tools/toolExecution.ts streamedCheckPermissionsAndCallTool",
        "src/services/tools/toolExecution.ts tengu_tool_use_progress",
        "src/services/tools/toolExecution.ts SLOW_PHASE_LOG_THRESHOLD_MS",
        "src/services/tools/toolExecution.ts preToolHookDurationMs",
        "src/services/tools/toolExecution.ts permissionDurationMs",
        "src/services/tools/toolExecution.ts postToolHookDurationMs",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_tool_phase_uses_claude_slow_phase_thresholds() {
        assert!(!model_tool_slow_phase_observed("pre_tool_use_hooks", 1_999));
        assert!(model_tool_slow_phase_observed("pre_tool_use_hooks", 2_000));
        assert!(model_tool_slow_phase_observed("permission_decision", 2_000));
        assert!(model_tool_slow_phase_observed("post_tool_use_hooks", 2_000));
        assert!(!model_tool_slow_phase_observed("tool_execution", 2_000));
    }

    #[test]
    fn model_tool_phase_marks_hook_timing_display_above_500ms() {
        let store = RunStore::new(test_tmp_root().join("coder-model-tool-phase-test"));
        let mut recorder =
            ModelToolPhaseRecorder::new(&store, "toolu_1", "Bash", "command_run", &json!({}));

        recorder.record_phase("pre_tool_use_hooks", "completed", 500, json!({}));
        recorder.record_phase("post_tool_use_hooks", "completed", 501, json!({}));
        let phases = recorder.into_phases();

        assert_eq!(
            phases[0]["hook_timing_display_threshold_ms"],
            json!(CLAUDE_HOOK_TIMING_DISPLAY_THRESHOLD_MS)
        );
        assert_eq!(phases[0]["show_inline_timing_summary"], json!(false));
        assert_eq!(phases[1]["show_inline_timing_summary"], json!(true));
    }

    fn test_tmp_root() -> std::path::PathBuf {
        std::env::var_os("CODER_TEST_TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }
}
