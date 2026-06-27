use std::{env, time::Duration};

use coder_core::RunId;
use coder_events::CoderEvent;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

const CONVERSATIONS_PATH: &str = "/api/conversations";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenHandsServerConfig {
    pub server_url: String,
    pub session_api_key_env: Option<String>,
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
        let response = self
            .send_json(
                "POST",
                CONVERSATIONS_PATH,
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
        let path = format!("{CONVERSATIONS_PATH}/{conversation_id}");
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
        let mut payload = json!({
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": message,
                    "cache_prompt": false
                }
            ],
            "run": false
        });
        if let Some(sender) = sender {
            payload["sender"] = Value::String(sender.to_owned());
        }

        let path = format!("{CONVERSATIONS_PATH}/{conversation_id}/events");
        self.send_json(
            "POST",
            &path,
            payload,
            &[StatusCode::OK, StatusCode::CREATED],
        )
        .await
    }

    pub async fn trigger_run(
        &self,
        conversation_id: &str,
    ) -> Result<OpenHandsRunTrigger, OpenHandsError> {
        let path = format!("{CONVERSATIONS_PATH}/{conversation_id}/run");
        let response = self
            .send_raw(
                "POST",
                &path,
                None,
                &[
                    StatusCode::OK,
                    StatusCode::CREATED,
                    StatusCode::NO_CONTENT,
                    StatusCode::CONFLICT,
                ],
            )
            .await?;
        Ok(OpenHandsRunTrigger {
            already_running: response.status == StatusCode::CONFLICT.as_u16(),
            status: response.status,
        })
    }

    pub async fn fetch_events(
        &self,
        conversation_id: &str,
        limit: u16,
    ) -> Result<Vec<Value>, OpenHandsError> {
        let mut events = Vec::new();
        let mut page_id: Option<String> = None;
        loop {
            let path = format!("{CONVERSATIONS_PATH}/{conversation_id}/events/search");
            let mut request = self
                .with_auth(self.client.get(self.url(&path)))
                .query(&[("limit", limit.to_string())]);
            if let Some(page_id) = &page_id {
                request = request.query(&[("page_id", page_id)]);
            }
            let response = request.send().await?;
            let response = checked_response("GET", &path, response, &[StatusCode::OK]).await?;
            let value = response.json;
            let items = value
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| OpenHandsError::InvalidResponse {
                    detail: "events search response missing items array".to_owned(),
                })?;
            events.extend(items.iter().cloned());
            page_id = value
                .get("next_page_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if page_id.is_none() {
                break;
            }
        }
        Ok(events)
    }

    pub fn events_websocket_url(&self, conversation_id: &str) -> Result<String, OpenHandsError> {
        let base = self.config.server_url.trim_end_matches('/');
        let url = if let Some(rest) = base.strip_prefix("https://") {
            format!("wss://{rest}/sockets/events/{conversation_id}")
        } else if let Some(rest) = base.strip_prefix("http://") {
            format!("ws://{rest}/sockets/events/{conversation_id}")
        } else {
            return Err(OpenHandsError::InvalidConfig(
                "server_url must start with http:// or https://".to_owned(),
            ));
        };
        if let Some(api_key) = self.session_api_key() {
            Ok(format!(
                "{url}?session_api_key={}",
                percent_encode_query_value(&api_key)
            ))
        } else {
            Ok(url)
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.config.server_url.trim_end_matches('/'), path)
    }

    fn with_auth(&self, request: RequestBuilder) -> RequestBuilder {
        if let Some(api_key) = self.session_api_key() {
            request.header("X-Session-API-Key", api_key)
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

pub fn normalize_openhands_event(
    run_id: RunId,
    sequence: u64,
    raw: Value,
    raw_ref: Option<String>,
) -> CoderEvent {
    let raw_kind = raw_event_kind(&raw);
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
    let event = CoderEvent::new(
        run_id,
        sequence,
        event_kind,
        json!({
            "backend": "openhands",
            "raw_event_id": raw_event_id,
            "raw_kind": raw_kind,
            "raw": raw
        }),
    );
    if let Some(raw_ref) = raw_ref {
        event.with_ref("openhands.raw_event", raw_ref)
    } else {
        event
    }
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
        return Err(OpenHandsError::HttpStatus {
            method,
            path: path.to_owned(),
            status: status.as_u16(),
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

fn raw_event_kind(raw: &Value) -> String {
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

fn percent_encode_query_value(value: &str) -> String {
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

    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn health_checks_health_endpoint() {
        let (server_url, requests) = spawn_server(vec![json_response(r#"{"status":"ok"}"#)]);
        let client = OpenHandsClient::new(OpenHandsServerConfig {
            server_url,
            session_api_key_env: None,
        });

        let health = client.health().await.unwrap();

        assert!(health.available);
        assert!(requests.lock().unwrap()[0].starts_with("GET /health "));
    }

    #[tokio::test]
    async fn create_send_run_and_fetch_events_use_agent_server_paths() {
        let (server_url, requests) = spawn_server(vec![
            json_response(r#"{"id":"conv-1"}"#),
            json_response(r#"{"accepted":true}"#),
            empty_response(204),
            json_response(
                r#"{"items":[{"id":"raw-1","type":"MessageEvent","api_key":"secret"}],"next_page_id":null}"#,
            ),
        ]);
        let client = OpenHandsClient::new(OpenHandsServerConfig {
            server_url,
            session_api_key_env: None,
        });

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
        assert!(!trigger.already_running);
        assert_eq!(events.len(), 1);
        let request_log = requests.lock().unwrap().join("\n---\n");
        assert!(request_log.contains("POST /api/conversations "));
        assert!(request_log.contains("POST /api/conversations/conv-1/events "));
        assert!(request_log.contains("POST /api/conversations/conv-1/run "));
        assert!(request_log.contains("GET /api/conversations/conv-1/events/search?limit=100 "));
    }

    #[test]
    fn websocket_url_uses_agent_server_socket_path() {
        let client = OpenHandsClient::new(OpenHandsServerConfig {
            server_url: "https://agent.example.test/root".to_owned(),
            session_api_key_env: None,
        });

        let url = client.events_websocket_url("conv-1").unwrap();

        assert_eq!(url, "wss://agent.example.test/root/sockets/events/conv-1");
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
        assert_eq!(event.payload["raw"]["api_key"], "[REDACTED]");
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
            .find_map(|line| line.strip_prefix("content-length: "))
            .and_then(|value| value.trim().parse::<usize>().ok())
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
}
