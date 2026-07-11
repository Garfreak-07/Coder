use axum::http::StatusCode;

use crate::provider_runtime::redact_provider_error;

pub(crate) const PLANNER_PROMPT_OVERFLOW_RECOVERY_ATTEMPTS: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlannerProviderRequestMode {
    Normal,
    PromptOverflowRecovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannerPromptTooLongError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannerProviderErrorBody {
    pub(crate) raw: String,
    pub(crate) redacted: String,
}

pub(crate) async fn read_planner_provider_error_body(
    response: reqwest::Response,
    redaction_values: &[&str],
) -> Result<PlannerProviderErrorBody, String> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| redact_provider_error(&error.to_string(), redaction_values))?;
    let raw = if bytes.len() > crate::PLANNER_PROVIDER_RESPONSE_MAX_BYTES {
        format!(
            "provider error body exceeded {} byte retention limit",
            crate::PLANNER_PROVIDER_RESPONSE_MAX_BYTES
        )
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let redacted = redact_provider_error(&raw, redaction_values);
    Ok(PlannerProviderErrorBody { raw, redacted })
}

pub(crate) fn planner_provider_error_is_prompt_too_long(status: StatusCode, body: &str) -> bool {
    if !matches!(status.as_u16(), 400 | 413) {
        return false;
    }
    let body = body.to_ascii_lowercase();
    body.contains("prompt is too long")
        || body.contains("prompt too long")
        || body.contains("context_length_exceeded")
        || body.contains("maximum context length")
        || body.contains("context length")
        || body.contains("too many tokens")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_too_long_detection_requires_recoverable_status() {
        assert!(planner_provider_error_is_prompt_too_long(
            StatusCode::BAD_REQUEST,
            "prompt is too long: 300000 tokens > 200000 maximum"
        ));
        assert!(planner_provider_error_is_prompt_too_long(
            StatusCode::PAYLOAD_TOO_LARGE,
            "context_length_exceeded"
        ));
        assert!(!planner_provider_error_is_prompt_too_long(
            StatusCode::UNAUTHORIZED,
            "prompt is too long"
        ));
        assert!(!planner_provider_error_is_prompt_too_long(
            StatusCode::BAD_REQUEST,
            "invalid model"
        ));
    }
}
