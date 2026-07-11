use coder_core::RunId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;

pub const DEFAULT_LARGE_PAYLOAD_PREVIEW_LIMIT: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRef {
    pub label: String,
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LargePayloadRef {
    pub preview: String,
    pub truncated: bool,
    pub blob_ref: String,
}

impl LargePayloadRef {
    pub fn from_text(text: &str, blob_ref: impl Into<String>, preview_limit: usize) -> Self {
        let mut chars = text.chars();
        let preview: String = chars.by_ref().take(preview_limit).collect();
        let truncated = chars.next().is_some();
        Self {
            preview,
            truncated,
            blob_ref: blob_ref.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderEvent {
    pub event_id: String,
    pub run_id: RunId,
    pub sequence: u64,
    #[serde(with = "time::serde::rfc3339")]
    pub timestamp: OffsetDateTime,
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub refs: Vec<EventRef>,
}

impl CoderEvent {
    pub fn new(run_id: RunId, sequence: u64, kind: impl Into<String>, payload: Value) -> Self {
        Self {
            event_id: format!("evt_{}", uuid::Uuid::new_v4()),
            run_id,
            sequence,
            timestamp: OffsetDateTime::now_utc(),
            kind: kind.into(),
            payload: redact_payload(payload),
            refs: Vec::new(),
        }
    }

    pub fn with_ref(mut self, label: impl Into<String>, uri: impl Into<String>) -> Self {
        self.refs.push(EventRef {
            label: label.into(),
            uri: uri.into(),
        });
        self
    }

    pub fn to_jsonl(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self).map(|line| format!("{line}\n"))
    }

    pub fn from_jsonl_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

pub fn redact_payload(value: Value) -> Value {
    match value {
        Value::Object(object) => {
            let redacted = object
                .into_iter()
                .map(|(key, value)| {
                    let value = if is_secret_key(&key) && !is_numeric_token_metric(&key, &value) {
                        Value::String("[REDACTED]".to_owned())
                    } else {
                        redact_payload(value)
                    };
                    (key, value)
                })
                .collect::<Map<String, Value>>();
            Value::Object(redacted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_payload).collect()),
        Value::String(text) => Value::String(redact_secret_text(&text)),
        other => other,
    }
}

pub fn redact_secret_text(text: &str) -> String {
    if contains_secret_marker(text) || contains_api_key_like_token(text) {
        "[REDACTED]".to_owned()
    } else {
        text.to_owned()
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized.contains("api_key")
        || normalized.contains("api-key")
        || normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("bearer")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("private_key")
        || normalized.contains("aws_access_key_id")
        || normalized.contains("aws_secret_access_key")
        || normalized.contains("authorization")
        || normalized.contains("cookie")
}

fn is_numeric_token_metric(key: &str, value: &Value) -> bool {
    let normalized = key.to_ascii_lowercase();
    (value.is_number() || value.is_null())
        && (normalized.ends_with("_tokens")
            || normalized.ends_with("_token_count")
            || normalized.ends_with("_token_budget")
            || normalized == "tokens")
}

fn contains_secret_marker(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    [
        "api_key",
        "api-key",
        "apikey",
        "authorization",
        "bearer ",
        "cookie",
        "password",
        "private_key",
        "secret",
        "token",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn contains_api_key_like_token(text: &str) -> bool {
    text.split(|ch: char| ch.is_whitespace())
        .map(|token| {
            token.trim_matches(|ch: char| {
                !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '_' | '.')
            })
        })
        .any(|token| {
            let normalized = token.to_ascii_lowercase();
            token.len() >= 12
                && (normalized.starts_with("sk-")
                    || normalized.starts_with("sk_")
                    || normalized.starts_with("sk_live_")
                    || normalized.starts_with("sk-proj-"))
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn event_roundtrips_as_jsonl() {
        let run_id = RunId::from_string("run_1");
        let event = CoderEvent::new(run_id, 1, "run.started", json!({"workflow_id": "wf"}));

        let line = event.to_jsonl().unwrap();
        let decoded = CoderEvent::from_jsonl_line(line.trim()).unwrap();

        assert_eq!(decoded.sequence, 1);
        assert_eq!(decoded.kind, "run.started");
        assert_eq!(decoded.payload["workflow_id"], "wf");
    }

    #[test]
    fn secret_like_payload_keys_are_redacted() {
        let event = CoderEvent::new(
            RunId::from_string("run_1"),
            1,
            "backend.connected",
            json!({
                "api_key": "sk-live",
                "nested": {
                    "session_token": "token-value",
                    "Authorization": "Bearer secret",
                    "cookie": "sid=secret",
                    "safe": "visible"
                }
            }),
        );

        assert_eq!(event.payload["api_key"], "[REDACTED]");
        assert_eq!(event.payload["nested"]["session_token"], "[REDACTED]");
        assert_eq!(event.payload["nested"]["Authorization"], "[REDACTED]");
        assert_eq!(event.payload["nested"]["cookie"], "[REDACTED]");
        assert_eq!(event.payload["nested"]["safe"], "visible");
    }

    #[test]
    fn numeric_token_metrics_remain_visible_but_string_tokens_are_redacted() {
        let event = CoderEvent::new(
            RunId::from_string("run_1"),
            1,
            "model.provider_turn.completed",
            json!({
                "input_tokens": 1200,
                "estimated_output_tokens": 300,
                "session_token": "token-value",
                "input_tokens_text": "1200"
            }),
        );

        assert_eq!(event.payload["input_tokens"], 1200);
        assert_eq!(event.payload["estimated_output_tokens"], 300);
        assert_eq!(event.payload["session_token"], "[REDACTED]");
        assert_eq!(event.payload["input_tokens_text"], "[REDACTED]");
    }

    #[test]
    fn secret_like_payload_strings_are_redacted() {
        let event = CoderEvent::new(
            RunId::from_string("run_1"),
            1,
            "run.started",
            json!({
                "task": "Use sk-live-1234567890 for the request",
                "note": "safe value"
            }),
        );

        assert_eq!(event.payload["task"], "[REDACTED]");
        assert_eq!(event.payload["note"], "safe value");
    }

    #[test]
    fn large_payload_ref_keeps_preview_and_blob_reference() {
        let payload = LargePayloadRef::from_text("abcdef", "blob://sha256/test", 3);

        assert_eq!(payload.preview, "abc");
        assert!(payload.truncated);
        assert_eq!(payload.blob_ref, "blob://sha256/test");
    }
}
