use std::{env, time::Duration};

use coder_core::{FinalReport, RunId};
use coder_events::CoderEvent;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenHandsServerConfig {
    pub server_url: String,
    pub session_api_key_env: Option<String>,
    #[serde(default)]
    pub api_paths: OpenHandsApiPaths,
    #[serde(default)]
    pub run_start_strategy: OpenHandsRunStartStrategy,
}

impl OpenHandsServerConfig {
    pub fn new(server_url: impl Into<String>, session_api_key_env: Option<String>) -> Self {
        Self {
            server_url: server_url.into(),
            session_api_key_env,
            api_paths: OpenHandsApiPaths::default(),
            run_start_strategy: OpenHandsRunStartStrategy::default(),
        }
    }

    pub fn legacy_sdk(server_url: impl Into<String>, session_api_key_env: Option<String>) -> Self {
        Self {
            server_url: server_url.into(),
            session_api_key_env,
            api_paths: OpenHandsApiPaths::legacy_sdk(),
            run_start_strategy: OpenHandsRunStartStrategy::PostRunEndpoint,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenHandsApiPaths {
    #[serde(default)]
    pub api_prefix: String,
    #[serde(default = "default_conversations_path")]
    pub conversations_path: String,
    #[serde(default)]
    pub events_search_path: Option<String>,
    #[serde(default)]
    pub run_endpoint_path: Option<String>,
    #[serde(default)]
    pub websocket_path_template: Option<String>,
    #[serde(default)]
    pub auth_header: OpenHandsAuthHeaderMode,
}

impl Default for OpenHandsApiPaths {
    fn default() -> Self {
        Self {
            api_prefix: String::new(),
            conversations_path: default_conversations_path(),
            events_search_path: None,
            run_endpoint_path: None,
            websocket_path_template: None,
            auth_header: OpenHandsAuthHeaderMode::default(),
        }
    }
}

impl OpenHandsApiPaths {
    pub fn legacy_sdk() -> Self {
        Self {
            api_prefix: "/api".to_owned(),
            conversations_path: default_conversations_path(),
            events_search_path: Some("/conversations/{conversation_id}/events/search".to_owned()),
            run_endpoint_path: Some("/conversations/{conversation_id}/run".to_owned()),
            websocket_path_template: Some("/sockets/events/{conversation_id}".to_owned()),
            auth_header: OpenHandsAuthHeaderMode::XSessionApiKey,
        }
    }
}

fn default_conversations_path() -> String {
    "/conversations".to_owned()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenHandsAuthHeaderMode {
    #[default]
    AuthorizationBearer,
    XSessionApiKey,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenHandsRunStartStrategy {
    PostRunEndpoint,
    #[default]
    PostUserEventWithRunTrue,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenHandsHealth {
    pub server_url: String,
    pub available: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenHandsConversation {
    pub id: String,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenHandsRunTrigger {
    pub already_running: bool,
    pub status: u16,
    pub strategy: OpenHandsRunStartStrategy,
}

pub struct OpenHandsClient {
    config: OpenHandsServerConfig,
    client: reqwest::Client,
}

impl OpenHandsClient {
    pub fn new(config: OpenHandsServerConfig) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .no_proxy()
            .build()
            .unwrap_or_else(|_| Client::new());
        Self { config, client }
    }

    pub async fn health(&self) -> Result<OpenHandsHealth, OpenHandsError> {
        let url = self.url("/health");
        let response = self.with_auth(self.client.get(url)).send().await;
        match response {
            Ok(response) => {
                let status = response.status();
                Ok(OpenHandsHealth {
                    server_url: self.config.server_url.clone(),
                    available: status.is_success(),
                    detail: format!("HTTP {status}"),
                })
            }
            Err(error) => Ok(OpenHandsHealth {
                server_url: self.config.server_url.clone(),
                available: false,
                detail: error.to_string(),
            }),
        }
    }

    pub async fn create_conversation(
        &self,
        payload: Value,
    ) -> Result<OpenHandsConversation, OpenHandsError> {
        let path = self.conversations_path();
        let response = self
            .send_json(
                "POST",
                &path,
                payload,
                &[StatusCode::OK, StatusCode::CREATED],
            )
            .await?;
        conversation_from_response(response)
    }

    pub async fn attach_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<OpenHandsConversation, OpenHandsError> {
        let path = self.conversation_path(conversation_id);
        let response = self.send_empty("GET", &path, &[StatusCode::OK]).await?;
        Ok(OpenHandsConversation {
            id: conversation_id.to_owned(),
            raw: response,
        })
    }

    pub async fn send_user_message(
        &self,
        conversation_id: &str,
        message: &str,
        sender: Option<&str>,
    ) -> Result<Value, OpenHandsError> {
        let start_run =
            self.config.run_start_strategy == OpenHandsRunStartStrategy::PostUserEventWithRunTrue;
        self.send_user_event(conversation_id, message, sender, start_run)
            .await
    }

    pub async fn trigger_run(
        &self,
        conversation_id: &str,
    ) -> Result<OpenHandsRunTrigger, OpenHandsError> {
        match self.config.run_start_strategy {
            OpenHandsRunStartStrategy::PostRunEndpoint => {
                let path = self.run_endpoint_path(conversation_id);
                let response = self
                    .send_raw(
                        "POST",
                        &path,
                        None,
                        &[
                            StatusCode::OK,
                            StatusCode::CREATED,
                            StatusCode::ACCEPTED,
                            StatusCode::NO_CONTENT,
                            StatusCode::CONFLICT,
                        ],
                    )
                    .await?;
                Ok(OpenHandsRunTrigger {
                    already_running: response.status == StatusCode::CONFLICT.as_u16(),
                    status: response.status,
                    strategy: OpenHandsRunStartStrategy::PostRunEndpoint,
                })
            }
            OpenHandsRunStartStrategy::PostUserEventWithRunTrue => Ok(OpenHandsRunTrigger {
                already_running: false,
                status: StatusCode::ACCEPTED.as_u16(),
                strategy: OpenHandsRunStartStrategy::PostUserEventWithRunTrue,
            }),
            OpenHandsRunStartStrategy::None => Ok(OpenHandsRunTrigger {
                already_running: false,
                status: 0,
                strategy: OpenHandsRunStartStrategy::None,
            }),
        }
    }

    pub async fn fetch_events(
        &self,
        conversation_id: &str,
        limit: u16,
    ) -> Result<Vec<Value>, OpenHandsError> {
        let mut events = Vec::new();
        let mut page_id: Option<String> = None;
        loop {
            let path = self.events_fetch_path(conversation_id);
            let mut request = self.with_auth(self.client.get(self.url(&path)));
            let paged_search = self.config.api_paths.events_search_path.is_some();
            if paged_search {
                request = request.query(&[("limit", limit.to_string())]);
                if let Some(page_id) = &page_id {
                    request = request.query(&[("page_id", page_id)]);
                }
            }
            let response = request.send().await?;
            let response = checked_response("GET", &path, response, &[StatusCode::OK]).await?;
            let page = parse_openhands_events_response(response.json)?;
            events.extend(page.items);
            page_id = page.next_page_id;
            if !paged_search || page_id.is_none() {
                break;
            }
        }
        Ok(events)
    }

    pub fn events_websocket_url(&self, conversation_id: &str) -> Result<String, OpenHandsError> {
        let base = self.config.server_url.trim_end_matches('/');
        let websocket_base = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            return Err(OpenHandsError::InvalidConfig(
                "server_url must start with http:// or https://".to_owned(),
            ));
        };
        Ok(format!(
            "{}{}",
            websocket_base.trim_end_matches('/'),
            self.websocket_path(conversation_id)
        ))
    }

    async fn send_user_event<'a>(
        &'a self,
        conversation_id: &'a str,
        message: &'a str,
        sender: Option<&'a str>,
        run: bool,
    ) -> Result<Value, OpenHandsError> {
        let mut payload = json!({
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": message,
                    "cache_prompt": false
                }
            ],
            "run": run
        });
        if let Some(sender) = sender {
            payload["sender"] = Value::String(sender.to_owned());
        }

        let path = self.events_post_path(conversation_id);
        self.send_json(
            "POST",
            &path,
            payload,
            &[
                StatusCode::OK,
                StatusCode::CREATED,
                StatusCode::ACCEPTED,
                StatusCode::NO_CONTENT,
            ],
        )
        .await
    }

    fn conversations_path(&self) -> String {
        prefixed_path(
            &self.config.api_paths.api_prefix,
            &self.config.api_paths.conversations_path,
        )
    }

    fn conversation_path(&self, conversation_id: &str) -> String {
        format!(
            "{}/{}",
            self.conversations_path().trim_end_matches('/'),
            percent_encode_path_segment(conversation_id)
        )
    }

    fn events_post_path(&self, conversation_id: &str) -> String {
        format!(
            "{}/events",
            self.conversation_path(conversation_id)
                .trim_end_matches('/')
        )
    }

    fn events_fetch_path(&self, conversation_id: &str) -> String {
        if let Some(template) = &self.config.api_paths.events_search_path {
            return self.template_path(template, conversation_id);
        }
        self.events_post_path(conversation_id)
    }

    fn run_endpoint_path(&self, conversation_id: &str) -> String {
        if let Some(template) = &self.config.api_paths.run_endpoint_path {
            return self.template_path(template, conversation_id);
        }
        format!(
            "{}/run",
            self.conversation_path(conversation_id)
                .trim_end_matches('/')
        )
    }

    fn websocket_path(&self, conversation_id: &str) -> String {
        if let Some(template) = &self.config.api_paths.websocket_path_template {
            let path = template.replace(
                "{conversation_id}",
                &percent_encode_path_segment(conversation_id),
            );
            return normalize_path(&path);
        }
        self.template_path(
            "/conversations/{conversation_id}/events/socket",
            conversation_id,
        )
    }

    fn template_path(&self, template: &str, conversation_id: &str) -> String {
        let path = template.replace(
            "{conversation_id}",
            &percent_encode_path_segment(conversation_id),
        );
        prefixed_path(&self.config.api_paths.api_prefix, &path)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.server_url.trim_end_matches('/'), path)
    }

    fn with_auth(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(api_key) = self.session_api_key() {
            match self.config.api_paths.auth_header {
                OpenHandsAuthHeaderMode::AuthorizationBearer => request.bearer_auth(api_key),
                OpenHandsAuthHeaderMode::XSessionApiKey => {
                    request.header("X-Session-API-Key", api_key)
                }
            }
        } else {
            request
        }
    }

    fn session_api_key(&self) -> Option<String> {
        self.config
            .session_api_key_env
            .as_deref()
            .and_then(|name| env::var(name).ok())
            .filter(|value| !value.trim().is_empty())
    }

    async fn send_empty(
        &self,
        method: &'static str,
        path: &str,
        acceptable: &[StatusCode],
    ) -> Result<Value, OpenHandsError> {
        let response = self.send_raw(method, path, None, acceptable).await?;
        Ok(response.json)
    }

    async fn send_json(
        &self,
        method: &'static str,
        path: &str,
        payload: Value,
        acceptable: &[StatusCode],
    ) -> Result<Value, OpenHandsError> {
        let response = self
            .send_raw(method, path, Some(payload), acceptable)
            .await?;
        Ok(response.json)
    }

    async fn send_raw(
        &self,
        method: &'static str,
        path: &str,
        payload: Option<Value>,
        acceptable: &[StatusCode],
    ) -> Result<OpenHandsHttpResponse, OpenHandsError> {
        let request = match method {
            "GET" => self.client.get(self.url(path)),
            "POST" => self.client.post(self.url(path)),
            other => return Err(OpenHandsError::InvalidMethod(other.to_owned())),
        };
        let request = self.with_auth(request);
        let request = if let Some(payload) = payload {
            request.json(&payload)
        } else {
            request
        };
        let response = request.send().await?;
        checked_response(method, path, response, acceptable).await
    }
}

pub fn parse_openhands_events_response(
    value: Value,
) -> Result<OpenHandsEventsPage, OpenHandsError> {
    if let Some(items) = value.as_array() {
        return Ok(OpenHandsEventsPage {
            items: items.clone(),
            next_page_id: None,
        });
    }
    let Some(object) = value.as_object() else {
        return Err(OpenHandsError::InvalidResponse {
            detail: "events response must be an array or object".to_owned(),
        });
    };
    if let Some(events) = object.get("events").and_then(Value::as_array) {
        return Ok(OpenHandsEventsPage {
            items: events.clone(),
            next_page_id: next_page_id(&value),
        });
    }
    if let Some(items) = object.get("items").and_then(Value::as_array) {
        return Ok(OpenHandsEventsPage {
            items: items.clone(),
            next_page_id: next_page_id(&value),
        });
    }
    Err(OpenHandsError::InvalidResponse {
        detail: "events response missing array, events array, or items array".to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenHandsEventsPage {
    pub items: Vec<Value>,
    pub next_page_id: Option<String>,
}

pub fn normalize_openhands_event(
    run_id: RunId,
    sequence: u64,
    raw: Value,
    raw_ref: Option<String>,
) -> CoderEvent {
    let raw_kind = openhands_raw_event_kind(&raw);
    let raw_event_id = raw
        .get("id")
        .or_else(|| raw.get("event_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let event_kind = if raw_kind == "unknown" {
        "backend.raw_event".to_owned()
    } else {
        format!("backend.openhands.{}", sanitize_event_kind(&raw_kind))
    };
    if let Some(raw_ref) = raw_ref {
        CoderEvent::new(
            run_id,
            sequence,
            event_kind,
            json!({
                "backend": "openhands",
                "raw_event_id": raw_event_id,
                "raw_kind": raw_kind,
                "raw_ref": raw_ref
            }),
        )
        .with_ref("openhands.raw_event", raw_ref)
    } else {
        CoderEvent::new(
            run_id,
            sequence,
            event_kind,
            json!({
                "backend": "openhands",
                "raw_event_id": raw_event_id,
                "raw_kind": raw_kind,
                "raw": raw
            }),
        )
    }
}

pub fn openhands_final_report(
    run_id: &RunId,
    conversation_id: &str,
    trigger: &OpenHandsRunTrigger,
    captured_events: usize,
    websocket_url: &str,
    raw_event_refs: &[String],
) -> FinalReport {
    let mut report = FinalReport::completed(format!(
        "OpenHands conversation '{conversation_id}' was triggered by the Rust adapter and {captured_events} event(s) were captured."
    ))
    .with_check(format!(
        "openhands trigger {:?} status {}",
        trigger.strategy, trigger.status
    ))
    .with_evidence(
        "event_log",
        format!("eventlog://runs/{}/events.jsonl", run_id.as_str()),
    )
    .with_evidence("openhands_conversation", conversation_id.to_owned())
    .with_evidence("openhands_events_websocket", websocket_url.to_owned());

    if trigger.already_running {
        report
            .checks
            .push("OpenHands reported an existing run was already active".to_owned());
    }
    for raw_ref in raw_event_refs.iter().take(10) {
        report.evidence_refs.push(coder_core::EvidenceRef {
            kind: "openhands_raw_event".to_owned(),
            reference: raw_ref.clone(),
        });
    }
    if raw_event_refs.len() > 10 {
        report.next_steps.push(format!(
            "{} additional raw OpenHands event ref(s) are available in the event log",
            raw_event_refs.len() - 10
        ));
    }
    if captured_events == 0 {
        report
            .next_steps
            .push("No OpenHands events were returned yet; fetch the conversation events again for more evidence.".to_owned());
    }
    report
}

#[derive(Debug)]
struct OpenHandsHttpResponse {
    status: u16,
    json: Value,
}

async fn checked_response(
    method: &'static str,
    path: &str,
    response: reqwest::Response,
    acceptable: &[StatusCode],
) -> Result<OpenHandsHttpResponse, OpenHandsError> {
    let status = response.status();
    let text = response.text().await?;
    if !acceptable.contains(&status) {
        let status = status.as_u16();
        if status == StatusCode::UNAUTHORIZED.as_u16() || status == StatusCode::FORBIDDEN.as_u16() {
            return Err(OpenHandsError::AuthFailure {
                method,
                path: path.to_owned(),
                status,
                body: text,
            });
        }
        if text.to_ascii_lowercase().contains("workspace") {
            return Err(OpenHandsError::WorkspaceError {
                method,
                path: path.to_owned(),
                status,
                body: text,
            });
        }
        return Err(OpenHandsError::HttpStatus {
            method,
            path: path.to_owned(),
            status,
            body: text,
        });
    }
    let json = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text)?
    };
    Ok(OpenHandsHttpResponse {
        status: status.as_u16(),
        json,
    })
}

fn conversation_from_response(value: Value) -> Result<OpenHandsConversation, OpenHandsError> {
    let id = value
        .get("id")
        .or_else(|| value.get("conversation_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| OpenHandsError::InvalidResponse {
            detail: "conversation response missing id or conversation_id".to_owned(),
        })?
        .to_owned();
    Ok(OpenHandsConversation { id, raw: value })
}

fn next_page_id(value: &Value) -> Option<String> {
    value
        .get("next_page_id")
        .or_else(|| value.get("next"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub fn openhands_raw_event_kind(raw: &Value) -> String {
    for key in ["kind", "type", "event_type", "action", "observation"] {
        if let Some(value) = raw.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                return value.to_owned();
            }
        }
    }
    "unknown".to_owned()
}

fn sanitize_event_kind(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn prefixed_path(api_prefix: &str, path: &str) -> String {
    let prefix = normalize_path(api_prefix);
    let path = normalize_path(path);
    if prefix.is_empty() {
        return path;
    }
    if path == prefix || path.starts_with(&format!("{prefix}/")) {
        path
    } else {
        format!(
            "{}/{}",
            prefix.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.starts_with('/') {
        trimmed.trim_end_matches('/').to_owned()
    } else {
        format!("/{}", trimmed.trim_end_matches('/'))
    }
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[derive(Debug, Error)]
pub enum OpenHandsError {
    #[error("invalid OpenHands config: {0}")]
    InvalidConfig(String),
    #[error("unsupported HTTP method: {0}")]
    InvalidMethod(String),
    #[error("OpenHands server request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("OpenHands authentication failed with HTTP {status} for {method} {path}: {body}")]
    AuthFailure {
        method: &'static str,
        path: String,
        status: u16,
        body: String,
    },
    #[error("OpenHands workspace error with HTTP {status} for {method} {path}: {body}")]
    WorkspaceError {
        method: &'static str,
        path: String,
        status: u16,
        body: String,
    },
    #[error("OpenHands server returned HTTP {status} for {method} {path}: {body}")]
    HttpStatus {
        method: &'static str,
        path: String,
        status: u16,
        body: String,
    },
    #[error("OpenHands JSON response error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid OpenHands response: {detail}")]
    InvalidResponse { detail: String },
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    use coder_core::{RunState, RunStatus, WorkflowId};
    use coder_store::RunStore;
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn health_checks_health_endpoint() {
        let (server_url, requests) = spawn_server(vec![json_response(r#"{"status":"ok"}"#)]);
        let client = OpenHandsClient::new(OpenHandsServerConfig::new(server_url, None));

        let health = client.health().await.unwrap();

        assert!(health.available);
        assert!(requests.lock().unwrap()[0].starts_with("GET /health "));
    }

    #[tokio::test]
    async fn default_paths_follow_agent_server_contract() {
        let (server_url, requests) = spawn_server(vec![
            json_response(r#"{"id":"conv-1"}"#),
            json_response(r#"{"id":"conv-1","status":"ready"}"#),
            json_response(r#"{"accepted":true}"#),
            json_response(r#"[{"id":"raw-1","type":"MessageEvent","api_key":"secret"}]"#),
        ]);
        let client = OpenHandsClient::new(OpenHandsServerConfig::new(server_url, None));

        let conversation = client
            .create_conversation(json!({"agent": {"kind": "test"}}))
            .await
            .unwrap();
        client.attach_conversation(&conversation.id).await.unwrap();
        client
            .send_user_message(&conversation.id, "hello", Some("coder"))
            .await
            .unwrap();
        let trigger = client.trigger_run(&conversation.id).await.unwrap();
        let events = client.fetch_events(&conversation.id, 100).await.unwrap();

        assert_eq!(conversation.id, "conv-1");
        assert_eq!(
            trigger.strategy,
            OpenHandsRunStartStrategy::PostUserEventWithRunTrue
        );
        assert_eq!(events.len(), 1);
        let request_log = requests.lock().unwrap().join("\n---\n");
        assert!(request_log.contains("POST /conversations "));
        assert!(request_log.contains("GET /conversations/conv-1 "));
        assert!(request_log.contains("POST /conversations/conv-1/events "));
        assert!(request_log.contains("\"run\":true"));
        assert!(request_log.contains("GET /conversations/conv-1/events "));
        assert!(!request_log.contains("/api/conversations"));
        assert!(!request_log.contains("/events/search"));
        assert!(!request_log.contains("/run "));
    }

    #[tokio::test]
    async fn legacy_path_config_keeps_sdk_compatibility() {
        let (server_url, requests) = spawn_server(vec![
            json_response(r#"{"id":"conv-1"}"#),
            json_response(r#"{"accepted":true}"#),
            empty_response(204),
            json_response(
                r#"{"items":[{"id":"raw-1","type":"MessageEvent"}],"next_page_id":null}"#,
            ),
        ]);
        let client = OpenHandsClient::new(OpenHandsServerConfig::legacy_sdk(server_url, None));

        let conversation = client
            .create_conversation(json!({"agent": {"kind": "test"}}))
            .await
            .unwrap();
        client
            .send_user_message(&conversation.id, "hello", Some("coder"))
            .await
            .unwrap();
        let trigger = client.trigger_run(&conversation.id).await.unwrap();
        let events = client.fetch_events(&conversation.id, 100).await.unwrap();

        assert_eq!(conversation.id, "conv-1");
        assert_eq!(trigger.strategy, OpenHandsRunStartStrategy::PostRunEndpoint);
        assert_eq!(events.len(), 1);
        let request_log = requests.lock().unwrap().join("\n---\n");
        assert!(request_log.contains("POST /api/conversations "));
        assert!(request_log.contains("POST /api/conversations/conv-1/events "));
        assert!(request_log.contains("\"run\":false"));
        assert!(request_log.contains("POST /api/conversations/conv-1/run "));
        assert!(request_log.contains("GET /api/conversations/conv-1/events/search?limit=100 "));
    }

    #[tokio::test]
    async fn auth_failures_are_classified_and_default_auth_uses_bearer_header() {
        let env_name = format!("CODER_TEST_SESSION_KEY_{}", unique_suffix());
        std::env::set_var(&env_name, "session-key");
        let (server_url, requests) = spawn_server(vec![response(
            401,
            "Unauthorized",
            r#"{"detail":"invalid session"}"#,
        )]);
        let client = OpenHandsClient::new(OpenHandsServerConfig::new(
            server_url,
            Some(env_name.clone()),
        ));

        let error = client.create_conversation(json!({})).await.unwrap_err();

        assert!(matches!(
            error,
            OpenHandsError::AuthFailure { status: 401, .. }
        ));
        assert!(requests.lock().unwrap()[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer session-key"));
        std::env::remove_var(env_name);
    }

    #[tokio::test]
    async fn legacy_auth_config_uses_session_api_key_header() {
        let env_name = format!("CODER_TEST_SESSION_KEY_{}", unique_suffix());
        std::env::set_var(&env_name, "session-key");
        let (server_url, requests) = spawn_server(vec![json_response(r#"{"id":"conv-1"}"#)]);
        let client = OpenHandsClient::new(OpenHandsServerConfig::legacy_sdk(
            server_url,
            Some(env_name.clone()),
        ));

        client.create_conversation(json!({})).await.unwrap();

        assert!(requests.lock().unwrap()[0]
            .to_ascii_lowercase()
            .contains("x-session-api-key: session-key"));
        std::env::remove_var(env_name);
    }

    #[test]
    fn response_parser_accepts_common_event_shapes() {
        let array = parse_openhands_events_response(json!([{"kind": "a"}])).unwrap();
        let events = parse_openhands_events_response(json!({"events": [{"kind": "b"}]})).unwrap();
        let items = parse_openhands_events_response(
            json!({"items": [{"kind": "c"}], "next_page_id": "p2"}),
        )
        .unwrap();
        let malformed = parse_openhands_events_response(json!({"unexpected": []})).unwrap_err();

        assert_eq!(array.items[0]["kind"], "a");
        assert_eq!(events.items[0]["kind"], "b");
        assert_eq!(items.items[0]["kind"], "c");
        assert_eq!(items.next_page_id.as_deref(), Some("p2"));
        assert!(matches!(malformed, OpenHandsError::InvalidResponse { .. }));
    }

    #[tokio::test]
    async fn workspace_failures_are_classified() {
        let (server_url, _) = spawn_server(vec![response(
            500,
            "Internal Server Error",
            r#"{"detail":"workspace is unavailable"}"#,
        )]);
        let client = OpenHandsClient::new(OpenHandsServerConfig::new(server_url, None));

        let error = client.attach_conversation("conv-1").await.unwrap_err();

        assert!(matches!(
            error,
            OpenHandsError::WorkspaceError { status: 500, .. }
        ));
    }

    #[tokio::test]
    async fn invalid_json_is_classified() {
        let (server_url, _) = spawn_server(vec![response(200, "OK", "{not json")]);
        let client = OpenHandsClient::new(OpenHandsServerConfig::new(server_url, None));

        let error = client.create_conversation(json!({})).await.unwrap_err();

        assert!(matches!(error, OpenHandsError::Json(_)));
    }

    #[tokio::test]
    async fn health_reports_unavailable_server_without_failing() {
        let client = OpenHandsClient::new(OpenHandsServerConfig::new("http://127.0.0.1:1", None));

        let health = client.health().await.unwrap();

        assert!(!health.available);
    }

    #[test]
    fn websocket_url_uses_default_agent_server_socket_path_and_base_path() {
        let client = OpenHandsClient::new(OpenHandsServerConfig::new(
            "https://agent.example.test/root",
            None,
        ));

        let url = client.events_websocket_url("conv-1").unwrap();

        assert_eq!(
            url,
            "wss://agent.example.test/root/conversations/conv-1/events/socket"
        );
    }

    #[test]
    fn websocket_url_uses_legacy_template_when_configured() {
        let client = OpenHandsClient::new(OpenHandsServerConfig::legacy_sdk(
            "http://127.0.0.1:8000/root",
            None,
        ));

        let url = client.events_websocket_url("conv-1").unwrap();

        assert_eq!(url, "ws://127.0.0.1:8000/root/sockets/events/conv-1");
    }

    #[test]
    fn websocket_url_rejects_invalid_server_url() {
        let client = OpenHandsClient::new(OpenHandsServerConfig::new("127.0.0.1:8000", None));

        let error = client.events_websocket_url("conv-1").unwrap_err();

        assert!(matches!(error, OpenHandsError::InvalidConfig(_)));
    }

    #[test]
    fn normalized_event_keeps_raw_ref_and_redacts_secret_like_payload() {
        let event = normalize_openhands_event(
            RunId::from_string("run_1"),
            7,
            json!({"id": "raw-1", "type": "MessageEvent", "api_key": "secret"}),
            Some("blob://sha256/raw".to_owned()),
        );

        assert_eq!(event.kind, "backend.openhands.MessageEvent");
        assert_eq!(event.refs[0].uri, "blob://sha256/raw");
        assert_eq!(event.payload["raw_ref"], "blob://sha256/raw");
        assert!(event.payload.get("raw").is_none());
    }

    #[test]
    fn openhands_report_includes_event_log_and_raw_event_evidence() {
        let run_id = RunId::from_string("run_1");
        let trigger = OpenHandsRunTrigger {
            already_running: true,
            status: 409,
            strategy: OpenHandsRunStartStrategy::PostRunEndpoint,
        };

        let report = openhands_final_report(
            &run_id,
            "conv-1",
            &trigger,
            2,
            "ws://127.0.0.1:8000/conversations/conv-1/events/socket",
            &[
                "blob://sha256/raw1".to_owned(),
                "blob://sha256/raw2".to_owned(),
            ],
        );

        assert!(report.summary.contains("conv-1"));
        assert!(report.checks.iter().any(|check| check.contains("409")));
        assert!(report
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "event_log"));
        assert!(report
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "openhands_raw_event"));
        assert!(report
            .checks
            .iter()
            .any(|check| check.contains("already active")));
    }

    #[tokio::test]
    #[ignore]
    async fn openhands_real_server_contract_smoke() {
        let server_url = env::var("OPENHANDS_AGENT_SERVER_URL")
            .expect("OPENHANDS_AGENT_SERVER_URL must point at an OpenHands Agent Server");
        let _session_key = env::var("OPENHANDS_SESSION_API_KEY")
            .expect("OPENHANDS_SESSION_API_KEY must contain a session API key");
        let create_payload = env::var("OPENHANDS_TEST_CREATE_PAYLOAD")
            .unwrap_or_else(|_| r#"{"agent":{"kind":"CodeActAgent"}}"#.to_owned());
        let payload = if create_payload.trim_start().starts_with('{') {
            serde_json::from_str(&create_payload).unwrap()
        } else {
            let text = std::fs::read_to_string(create_payload).unwrap();
            serde_json::from_str(&text).unwrap()
        };
        let client = OpenHandsClient::new(OpenHandsServerConfig::new(
            server_url,
            Some("OPENHANDS_SESSION_API_KEY".to_owned()),
        ));
        let health = client.health().await.unwrap();
        assert!(health.available, "server health failed: {}", health.detail);

        let conversation = client.create_conversation(payload).await.unwrap();
        client
            .send_user_message(
                &conversation.id,
                "Summarize the current workspace in one sentence.",
                Some("coder-rust-test"),
            )
            .await
            .unwrap();
        let trigger = client.trigger_run(&conversation.id).await.unwrap();
        let raw_events = client.fetch_events(&conversation.id, 100).await.unwrap();

        let root =
            std::env::temp_dir().join(format!("coder-openhands-smoke-{}", std::process::id()));
        let store = RunStore::new(&root);
        let run_id = RunId::new();
        let mut state = RunState::new(run_id.clone(), WorkflowId::new("openhands-smoke"));
        state.status = RunStatus::Running;
        store.write_metadata(&state).unwrap();
        for (index, raw) in raw_events.iter().cloned().enumerate() {
            let raw_ref = store
                .write_large_text_ref(&serde_json::to_string(&raw).unwrap())
                .unwrap()
                .blob_ref;
            let event =
                normalize_openhands_event(run_id.clone(), index as u64 + 1, raw, Some(raw_ref));
            store.append_event(&run_id, &event).unwrap();
        }
        let websocket_url = client.events_websocket_url(&conversation.id).unwrap();
        let report = openhands_final_report(
            &run_id,
            &conversation.id,
            &trigger,
            raw_events.len(),
            &websocket_url,
            &[],
        );
        store.write_report(&run_id, &report).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    fn spawn_server(responses: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_for_thread = Arc::clone(&requests);
        let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
        thread::spawn(move || {
            listener
                .set_nonblocking(false)
                .expect("listener should be blocking");
            while !responses.lock().unwrap().is_empty() {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let request = read_request(&mut stream);
                requests_for_thread.lock().unwrap().push(request);
                let response = responses.lock().unwrap().pop_front().unwrap();
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), requests)
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut buffer = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let read = stream.read(&mut chunk).unwrap_or(0);
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if request_is_complete(&buffer) {
                break;
            }
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }

    fn request_is_complete(buffer: &[u8]) -> bool {
        let Some(header_end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length: ")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        buffer.len() >= header_end + 4 + content_length
    }

    fn json_response(body: &str) -> String {
        response(200, "OK", body)
    }

    fn empty_response(status: u16) -> String {
        response(status, "OK", "")
    }

    fn response(status: u16, reason: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn unique_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
