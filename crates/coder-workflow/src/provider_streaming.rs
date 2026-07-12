use std::collections::BTreeMap;

use serde_json::Value;

use crate::model_tool_loop::ModelToolUseBlock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStreamEventKind {
    ContentDelta,
    ToolCallDelta,
    ToolCallReady,
    Finished,
    MalformedModelOutput,
    Aborted,
    Discarded,
}

impl ProviderStreamEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ContentDelta => "content_delta",
            Self::ToolCallDelta => "tool_call_delta",
            Self::ToolCallReady => "tool_call_ready",
            Self::Finished => "finished",
            Self::MalformedModelOutput => "malformed_model_output",
            Self::Aborted => "aborted",
            Self::Discarded => "discarded",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderStreamEvent {
    pub kind: ProviderStreamEventKind,
    pub content_delta: Option<String>,
    pub tool_use: Option<ModelToolUseBlock>,
    pub issue: Option<ProviderStreamIssue>,
    pub finish_reason: Option<String>,
}

impl ProviderStreamEvent {
    fn new(kind: ProviderStreamEventKind) -> Self {
        Self {
            kind,
            content_delta: None,
            tool_use: None,
            issue: None,
            finish_reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStreamIssue {
    pub code: String,
    pub message: String,
    pub tool_use_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderStreamFinal {
    pub assistant_content: String,
    pub finish_reason: Option<String>,
    pub tool_uses: Vec<ModelToolUseBlock>,
    pub issues: Vec<ProviderStreamIssue>,
    pub aborted: bool,
    pub discarded: bool,
}

#[derive(Debug, Clone, Default)]
pub struct OpenAiCompatibleStreamAdapter {
    assistant_content: String,
    tool_calls: BTreeMap<u64, ToolCallAccumulator>,
    tool_uses: Vec<ModelToolUseBlock>,
    issues: Vec<ProviderStreamIssue>,
    finish_reason: Option<String>,
    finished: bool,
    aborted: bool,
    discarded: bool,
}

impl OpenAiCompatibleStreamAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply_chunk(&mut self, chunk: &Value) -> Vec<ProviderStreamEvent> {
        if self.discarded || self.aborted {
            return Vec::new();
        }
        if self.finished {
            return vec![self.malformed_event(
                "chunk_after_terminal",
                "provider emitted a stream chunk after a terminal finish_reason",
                None,
            )];
        }

        let Some(choices) = chunk.get("choices").and_then(Value::as_array) else {
            return vec![self.malformed_event(
                "missing_choices",
                "provider stream chunk did not contain choices[]",
                None,
            )];
        };
        if choices.is_empty() {
            return vec![self.malformed_event(
                "empty_choices",
                "provider stream chunk contained an empty choices[] array",
                None,
            )];
        }

        let mut events = Vec::new();
        for choice in choices {
            if let Some(delta) = choice.get("delta") {
                self.apply_delta(delta, &mut events);
            }
            if let Some(finish_reason) = choice.get("finish_reason") {
                if !finish_reason.is_null() {
                    self.finish_reason = finish_reason
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| Some(finish_reason.to_string().trim_matches('"').to_owned()));
                    self.finished = true;
                }
            }
        }

        if self.finished {
            events.extend(self.finish_tool_calls());
            let mut finished = ProviderStreamEvent::new(ProviderStreamEventKind::Finished);
            finished.finish_reason = self.finish_reason.clone();
            events.push(finished);
        }

        events
    }

    pub fn abort(&mut self, reason: impl Into<String>) -> ProviderStreamEvent {
        let reason = reason.into();
        self.clear_buffers();
        self.aborted = true;
        let mut event = ProviderStreamEvent::new(ProviderStreamEventKind::Aborted);
        event.issue = Some(ProviderStreamIssue {
            code: "aborted".to_owned(),
            message: reason,
            tool_use_id: None,
        });
        event
    }

    pub fn discard_for_fallback(&mut self, reason: impl Into<String>) -> ProviderStreamEvent {
        let reason = reason.into();
        self.clear_buffers();
        self.discarded = true;
        let mut event = ProviderStreamEvent::new(ProviderStreamEventKind::Discarded);
        event.issue = Some(ProviderStreamIssue {
            code: "streaming_fallback_discarded".to_owned(),
            message: reason,
            tool_use_id: None,
        });
        event
    }

    pub fn final_state(&self) -> ProviderStreamFinal {
        ProviderStreamFinal {
            assistant_content: self.assistant_content.clone(),
            finish_reason: self.finish_reason.clone(),
            tool_uses: self.tool_uses.clone(),
            issues: self.issues.clone(),
            aborted: self.aborted,
            discarded: self.discarded,
        }
    }

    pub fn tracked_tool_call_count(&self) -> usize {
        self.tool_calls.len()
    }

    fn apply_delta(&mut self, delta: &Value, events: &mut Vec<ProviderStreamEvent>) {
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            if !content.is_empty() {
                self.assistant_content.push_str(content);
                let mut event = ProviderStreamEvent::new(ProviderStreamEventKind::ContentDelta);
                event.content_delta = Some(content.to_owned());
                events.push(event);
            }
        }

        let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) else {
            return;
        };
        for (position, tool_call) in tool_calls.iter().enumerate() {
            let index = tool_call
                .get("index")
                .and_then(Value::as_u64)
                .unwrap_or(position as u64);
            let accumulator = self.tool_calls.entry(index).or_default();
            if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    accumulator.id = Some(id.to_owned());
                }
            }
            if let Some(function) = tool_call.get("function") {
                if let Some(name) = function.get("name").and_then(Value::as_str) {
                    if !name.is_empty() {
                        accumulator.name = Some(name.to_owned());
                    }
                }
                if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                    accumulator.arguments.push_str(arguments);
                }
            }
            events.push(ProviderStreamEvent::new(
                ProviderStreamEventKind::ToolCallDelta,
            ));
        }
    }

    fn finish_tool_calls(&mut self) -> Vec<ProviderStreamEvent> {
        let mut events = Vec::new();
        let tool_calls = std::mem::take(&mut self.tool_calls);
        for (index, accumulator) in tool_calls {
            match accumulator.into_tool_use(index) {
                Ok(tool_use) => {
                    self.tool_uses.push(tool_use.clone());
                    let mut event =
                        ProviderStreamEvent::new(ProviderStreamEventKind::ToolCallReady);
                    event.tool_use = Some(tool_use);
                    events.push(event);
                }
                Err(issue) => {
                    self.issues.push(issue.clone());
                    let mut event =
                        ProviderStreamEvent::new(ProviderStreamEventKind::MalformedModelOutput);
                    event.issue = Some(issue);
                    events.push(event);
                }
            }
        }
        events
    }

    fn malformed_event(
        &mut self,
        code: &str,
        message: &str,
        tool_use_id: Option<String>,
    ) -> ProviderStreamEvent {
        let issue = ProviderStreamIssue {
            code: code.to_owned(),
            message: message.to_owned(),
            tool_use_id,
        };
        self.issues.push(issue.clone());
        let mut event = ProviderStreamEvent::new(ProviderStreamEventKind::MalformedModelOutput);
        event.issue = Some(issue);
        event
    }

    fn clear_buffers(&mut self) {
        self.assistant_content.clear();
        self.tool_calls.clear();
        self.tool_uses.clear();
        self.issues.clear();
        self.finish_reason = None;
        self.finished = false;
    }
}

#[derive(Debug, Clone, Default)]
struct ToolCallAccumulator {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

impl ToolCallAccumulator {
    fn into_tool_use(self, index: u64) -> Result<ModelToolUseBlock, ProviderStreamIssue> {
        let id = self
            .id
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| ProviderStreamIssue {
                code: "missing_tool_call_id".to_owned(),
                message: format!("streamed tool call at index {index} did not include an id"),
                tool_use_id: None,
            })?;
        let name = self
            .name
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| ProviderStreamIssue {
                code: "missing_tool_call_name".to_owned(),
                message: format!("streamed tool call '{id}' did not include a function name"),
                tool_use_id: Some(id.clone()),
            })?;
        let input = if self.arguments.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(&self.arguments).map_err(|error| ProviderStreamIssue {
                code: "invalid_tool_call_arguments".to_owned(),
                message: format!(
                    "streamed tool call '{id}' arguments were not valid JSON: {error}"
                ),
                tool_use_id: Some(id.clone()),
            })?
        };
        Ok(ModelToolUseBlock::new(id, name, input))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn provider_streaming_yields_partial_content_progress() {
        let mut adapter = OpenAiCompatibleStreamAdapter::new();

        let events = adapter.apply_chunk(&json!({
            "choices": [{
                "delta": {"content": "hel"},
                "finish_reason": null
            }]
        }));

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, ProviderStreamEventKind::ContentDelta);
        assert_eq!(events[0].content_delta.as_deref(), Some("hel"));

        let events = adapter.apply_chunk(&json!({
            "choices": [{
                "delta": {"content": "lo"},
                "finish_reason": "stop"
            }]
        }));

        assert_eq!(events[0].kind, ProviderStreamEventKind::ContentDelta);
        assert_eq!(events[1].kind, ProviderStreamEventKind::Finished);
        let final_state = adapter.final_state();
        assert_eq!(final_state.assistant_content, "hello");
        assert_eq!(final_state.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn provider_streaming_accumulates_tool_call_chunks_into_model_tool_use() {
        let mut adapter = OpenAiCompatibleStreamAdapter::new();

        adapter.apply_chunk(&json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "toolu_read",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\""
                        }
                    }]
                },
                "finish_reason": null
            }]
        }));
        let events = adapter.apply_chunk(&json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": ":\"README.md\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }));

        assert!(events
            .iter()
            .any(|event| event.kind == ProviderStreamEventKind::ToolCallReady));
        let final_state = adapter.final_state();
        assert_eq!(final_state.tool_uses.len(), 1);
        assert_eq!(final_state.tool_uses[0].id, "toolu_read");
        assert_eq!(final_state.tool_uses[0].name, "read_file");
        assert_eq!(final_state.tool_uses[0].input["path"], "README.md");
    }

    #[test]
    fn provider_streaming_abort_clears_buffers_and_ignores_later_chunks() {
        let mut adapter = OpenAiCompatibleStreamAdapter::new();
        adapter.apply_chunk(&json!({
            "choices": [{
                "delta": {
                    "content": "partial",
                    "tool_calls": [{
                        "index": 0,
                        "id": "toolu_read",
                        "function": {"name": "read_file", "arguments": "{}"}
                    }]
                },
                "finish_reason": null
            }]
        }));

        let event = adapter.abort("user cancelled provider stream");

        assert_eq!(event.kind, ProviderStreamEventKind::Aborted);
        assert_eq!(adapter.tracked_tool_call_count(), 0);
        assert!(adapter
            .apply_chunk(
                &json!({"choices": [{"delta": {"content": "late"}, "finish_reason": "stop"}]})
            )
            .is_empty());
        let final_state = adapter.final_state();
        assert!(final_state.aborted);
        assert_eq!(final_state.assistant_content, "");
        assert!(final_state.tool_uses.is_empty());
    }

    #[test]
    fn provider_streaming_discard_for_fallback_releases_attempt_state() {
        let mut adapter = OpenAiCompatibleStreamAdapter::new();
        adapter.apply_chunk(&json!({
            "choices": [{
                "delta": {
                    "content": "first attempt",
                    "tool_calls": [{
                        "index": 0,
                        "id": "toolu_read",
                        "function": {"name": "read_file", "arguments": "{\"path\":\""}
                    }]
                },
                "finish_reason": null
            }]
        }));

        let event = adapter.discard_for_fallback("provider retry switched to non-streaming");

        assert_eq!(event.kind, ProviderStreamEventKind::Discarded);
        assert_eq!(adapter.tracked_tool_call_count(), 0);
        assert!(adapter
            .apply_chunk(
                &json!({"choices": [{"delta": {"content": "late"}, "finish_reason": "stop"}]})
            )
            .is_empty());
        let final_state = adapter.final_state();
        assert!(final_state.discarded);
        assert_eq!(final_state.assistant_content, "");
        assert!(final_state.tool_uses.is_empty());
    }

    #[test]
    fn provider_streaming_reports_malformed_tool_call_arguments_without_tool_use() {
        let mut adapter = OpenAiCompatibleStreamAdapter::new();

        let events = adapter.apply_chunk(&json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "toolu_bad",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }));

        assert!(events
            .iter()
            .any(|event| event.kind == ProviderStreamEventKind::MalformedModelOutput));
        let final_state = adapter.final_state();
        assert!(final_state.tool_uses.is_empty());
        assert_eq!(final_state.issues.len(), 1);
        assert_eq!(final_state.issues[0].code, "invalid_tool_call_arguments");
        assert_eq!(
            final_state.issues[0].tool_use_id.as_deref(),
            Some("toolu_bad")
        );
    }
}
