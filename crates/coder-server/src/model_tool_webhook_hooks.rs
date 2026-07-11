use coder_config::{HookCommandSpec, HookEvent};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::model_tool_command_hooks::CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS;
use crate::model_tool_hook_output::{
    bounded_hook_output_preview, parse_model_tool_hook_output, ModelToolHookEffects,
    ParsedModelToolHookOutput,
};
use crate::model_tool_hook_phase::{hook_command_kind, hook_event_name, ModelToolHookContext};
use crate::model_tool_hook_runtime::ModelToolHookExecution;

pub(crate) async fn execute_webhook_model_tool_hook(
    hook: &HookCommandSpec,
    event: HookEvent,
    requested_tool_name: &str,
    hook_input: &Value,
    context: &ModelToolHookContext,
) -> ModelToolHookExecution {
    let HookCommandSpec::Webhook {
        url,
        timeout,
        headers,
        allowed_env_vars,
        ..
    } = hook
    else {
        return ModelToolHookExecution {
            payload: json!({
                "type": hook_command_kind(hook),
                "outcome": "unsupported"
            }),
            blocking_error: None,
            effects: ModelToolHookEffects::default(),
        };
    };
    let timeout_seconds = timeout.unwrap_or(CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS);
    let started = Instant::now();
    if let Err(error) = enforce_webhook_url_policy(url, context.allowed_webhook_urls.as_deref()) {
        return webhook_policy_error(
            url,
            event,
            requested_tool_name,
            timeout_seconds,
            started,
            "allowed_webhook_urls",
            error,
        );
    }
    let proxy_policy = webhook_proxy_policy(url);
    let ssrf_guard = if proxy_policy.uses_proxy() {
        None
    } else {
        match WebhookSsrfGuard::new(url) {
            Ok(check) => Some(check),
            Err(error) => {
                return webhook_policy_error(
                    url,
                    event,
                    requested_tool_name,
                    timeout_seconds,
                    started,
                    "ssrf_guard",
                    error,
                );
            }
        }
    };
    let mut client_builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_seconds))
        .redirect(reqwest::redirect::Policy::none());
    if let Some(proxy_url) = proxy_policy.proxy_url.as_deref() {
        let proxy = match reqwest::Proxy::all(proxy_url) {
            Ok(proxy) => proxy,
            Err(error) => {
                return ModelToolHookExecution {
                    payload: json!({
                        "type": "webhook",
                        "hook_transport": "webhook",
                        "url": url,
                        "hook_event": hook_event_name(event),
                        "tool_name": requested_tool_name,
                        "outcome": "execution_error",
                        "error": format!(
                            "webhook hook proxy URL from {} is invalid: {error}",
                            proxy_policy.proxy_source.unwrap_or("environment")
                        ),
                        "duration_ms": started.elapsed().as_millis() as u64,
                        "timeout_seconds": timeout_seconds,
                        "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS,
                        "transport": proxy_policy.report(),
                        "ssrf_guard": webhook_ssrf_report(ssrf_guard.as_ref(), &proxy_policy)
                    }),
                    blocking_error: None,
                    effects: ModelToolHookEffects::default(),
                };
            }
        };
        client_builder = client_builder.proxy(proxy);
    } else {
        client_builder = client_builder.no_proxy();
    }
    if let Some(ssrf_guard) = ssrf_guard.as_ref() {
        client_builder = client_builder.dns_resolver(Arc::new(ssrf_guard.resolver()));
    }
    let client = match client_builder.build() {
        Ok(client) => client,
        Err(error) => {
            return ModelToolHookExecution {
                payload: json!({
                    "type": "webhook",
                    "hook_transport": "webhook",
                    "url": url,
                    "hook_event": hook_event_name(event),
                    "tool_name": requested_tool_name,
                    "outcome": "execution_error",
                    "error": error.to_string(),
                    "transport": proxy_policy.report(),
                    "ssrf_guard": webhook_ssrf_report(ssrf_guard.as_ref(), &proxy_policy)
                }),
                blocking_error: None,
                effects: ModelToolHookEffects::default(),
            };
        }
    };
    let mut request = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(hook_input.to_string());
    let effective_allowed_env_vars = effective_webhook_allowed_env_vars(
        allowed_env_vars,
        context.webhook_allowed_env_vars.as_deref(),
    );
    for (name, value) in headers {
        let header_value = interpolate_webhook_header_value(value, &effective_allowed_env_vars);
        request = request.header(name.as_str(), header_value);
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            let timed_out = error.is_timeout();
            let error = error.to_string();
            if error.contains("webhook hook blocked:") {
                return webhook_policy_error(
                    url,
                    event,
                    requested_tool_name,
                    timeout_seconds,
                    started,
                    "ssrf_guard",
                    error,
                );
            }
            return ModelToolHookExecution {
                payload: json!({
                    "type": "webhook",
                    "url": url,
                    "hook_event": hook_event_name(event),
                    "tool_name": requested_tool_name,
                    "outcome": "execution_error",
                    "hook_output_kind": if timed_out { "request_timeout" } else { "request_error" },
                    "aborted": timed_out,
                    "error": if timed_out {
                        format!("webhook hook request timed out after {timeout_seconds} second(s): {error}")
                    } else {
                        error
                    },
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "timeout_seconds": timeout_seconds,
                    "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS,
                    "request_protocol": "claude.hook_input.v1",
                    "transport": proxy_policy.report(),
                    "ssrf_guard": webhook_ssrf_report(ssrf_guard.as_ref(), &proxy_policy)
                }),
                blocking_error: None,
                effects: ModelToolHookEffects::default(),
            };
        }
    };
    let status_code = response.status().as_u16();
    let ok = response.status().is_success();
    let body = match response.text().await {
        Ok(body) => body,
        Err(error) => {
            return ModelToolHookExecution {
                payload: json!({
                    "type": "webhook",
                    "hook_transport": "webhook",
                    "url": url,
                    "hook_event": hook_event_name(event),
                    "tool_name": requested_tool_name,
                    "outcome": "execution_error",
                    "status_code": status_code,
                    "error": error.to_string(),
                    "duration_ms": started.elapsed().as_millis() as u64,
                    "timeout_seconds": timeout_seconds,
                    "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS
                }),
                blocking_error: None,
                effects: ModelToolHookEffects::default(),
            };
        }
    };
    let (output_preview, output_truncated) = bounded_hook_output_preview(&body);
    if !ok {
        return ModelToolHookExecution {
            payload: json!({
                    "type": "webhook",
                    "hook_transport": "webhook",
                "url": url,
                "hook_event": hook_event_name(event),
                "tool_name": requested_tool_name,
                "outcome": "non_blocking_error",
                "status_code": status_code,
                "output_preview": output_preview,
                "output_truncated": output_truncated,
                "duration_ms": started.elapsed().as_millis() as u64,
                "timeout_seconds": timeout_seconds,
                "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS
            }),
            blocking_error: None,
            effects: ModelToolHookEffects::default(),
        };
    }

    let body_for_parse = if body.trim().is_empty() {
        "{}"
    } else {
        body.trim()
    };
    let parsed_output = if body_for_parse.starts_with('{') {
        parse_model_tool_hook_output(body_for_parse, event, url)
    } else {
        ParsedModelToolHookOutput {
            kind: "invalid_webhook_json",
            json_output: None,
            validation_error: Some(format!(
                "webhook hook must return JSON, but got non-JSON response body: {}",
                output_preview
            )),
            blocking_error: None,
            effects: ModelToolHookEffects::default(),
        }
    };
    let validation_error = parsed_output.validation_error.clone();
    let blocking_error = parsed_output.blocking_error.clone();
    let outcome = if blocking_error.is_some() {
        "blocking"
    } else if validation_error.is_some() {
        "non_blocking_error"
    } else {
        "success"
    };
    let effects = parsed_output.effects;
    ModelToolHookExecution {
        payload: json!({
            "type": "webhook",
            "hook_transport": "webhook",
            "url": url,
            "hook_event": hook_event_name(event),
            "tool_name": requested_tool_name,
            "outcome": outcome,
            "status_code": status_code,
            "duration_ms": started.elapsed().as_millis() as u64,
            "timeout_seconds": timeout_seconds,
            "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS,
            "request_protocol": "claude.hook_input.v1",
            "hook_output_kind": parsed_output.kind,
            "hook_json_output": parsed_output.json_output,
            "hook_output_validation_error": validation_error,
            "permission_behavior": effects.permission_behavior,
            "permission_decision_reason": effects.permission_decision_reason.clone(),
            "updated_input": effects.updated_input.clone(),
            "additional_context": effects.additional_context.clone(),
            "updated_tool_output": effects.updated_tool_output.clone(),
            "prevent_continuation": effects.prevent_continuation,
            "stop_reason": effects.stop_reason.clone(),
            "output_preview": output_preview,
            "output_truncated": output_truncated,
            "allowed_url_policy": webhook_url_policy_summary(context.allowed_webhook_urls.as_deref()),
            "transport": proxy_policy.report(),
            "ssrf_guard": webhook_ssrf_report(ssrf_guard.as_ref(), &proxy_policy),
            "effective_allowed_env_vars": effective_allowed_env_vars
        }),
        blocking_error,
        effects,
    }
}

fn webhook_policy_error(
    url: &str,
    event: HookEvent,
    requested_tool_name: &str,
    timeout_seconds: u64,
    started: Instant,
    policy: &'static str,
    error: String,
) -> ModelToolHookExecution {
    ModelToolHookExecution {
        payload: json!({
            "type": "webhook",
            "hook_transport": "webhook",
            "url": url,
            "hook_event": hook_event_name(event),
            "tool_name": requested_tool_name,
            "outcome": "non_blocking_error",
            "hook_output_kind": "webhook_policy_blocked",
            "webhook_output_kind": "webhook_policy_blocked",
            "hook_output_validation_error": error,
            "policy": policy,
            "duration_ms": started.elapsed().as_millis() as u64,
            "timeout_seconds": timeout_seconds,
            "default_timeout_seconds": CLAUDE_TOOL_HOOK_EXECUTION_TIMEOUT_SECONDS,
            "request_protocol": "claude.hook_input.v1",
            "claude_sources": [
                "src/utils/hooks/execHttpHook.ts getHttpHookPolicy",
                "src/utils/hooks/execHttpHook.ts urlMatchesPattern",
                "src/utils/hooks/ssrfGuard.ts"
            ]
        }),
        blocking_error: None,
        effects: ModelToolHookEffects::default(),
    }
}

pub(crate) fn enforce_webhook_url_policy(
    url: &str,
    allowed_urls: Option<&[String]>,
) -> Result<(), String> {
    enforce_webhook_transport_policy(url)?;
    let Some(allowed_urls) = allowed_urls else {
        return Ok(());
    };
    if allowed_urls
        .iter()
        .any(|pattern| wildcard_pattern_matches(url, pattern))
    {
        return Ok(());
    }
    Err(format!(
        "webhook hook blocked: {url} does not match any pattern in allowedWebhookUrls"
    ))
}

fn enforce_webhook_transport_policy(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|error| format!("webhook hook blocked: invalid URL: {error}"))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if webhook_url_targets_loopback(&parsed) => Ok(()),
        "http" => Err(format!(
            "webhook hook blocked: external hook URL {url} must use https://. http:// is allowed only for loopback local development (localhost, 127.0.0.1/8, or ::1)."
        )),
        scheme => Err(format!(
            "webhook hook blocked: unsupported URL scheme '{scheme}'. Use https:// for external hooks or http:// loopback for local development."
        )),
    }
}

fn webhook_url_targets_loopback(url: &reqwest::Url) -> bool {
    if url.scheme() != "http" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost" {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn wildcard_pattern_matches(value: &str, pattern: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return value == pattern;
    }
    let starts_with_wildcard = pattern.starts_with('*');
    let ends_with_wildcard = pattern.ends_with('*');
    let parts = pattern.split('*').collect::<Vec<_>>();
    let mut remaining = value;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 && !starts_with_wildcard {
            let Some(stripped) = remaining.strip_prefix(part) else {
                return false;
            };
            remaining = stripped;
            continue;
        }
        let Some(position) = remaining.find(part) else {
            return false;
        };
        remaining = &remaining[position + part.len()..];
    }
    if !ends_with_wildcard {
        if let Some(last) = parts.iter().rev().find(|part| !part.is_empty()) {
            return value.ends_with(last);
        }
    }
    true
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebhookProxyPolicy {
    pub(crate) proxy_url: Option<String>,
    pub(crate) proxy_source: Option<&'static str>,
    no_proxy_source: Option<&'static str>,
    bypassed: bool,
}

impl WebhookProxyPolicy {
    pub(crate) fn uses_proxy(&self) -> bool {
        self.proxy_url.is_some() && !self.bypassed
    }

    pub(crate) fn report(&self) -> Value {
        json!({
            "mode": if self.uses_proxy() {
                "env_proxy"
            } else if self.bypassed {
                "env_proxy_bypassed"
            } else {
                "direct"
            },
            "proxy_configured": self.proxy_source.is_some(),
            "proxy_source": self.proxy_source,
            "no_proxy_configured": self.no_proxy_source.is_some(),
            "no_proxy_source": self.no_proxy_source,
            "proxy_bypassed": self.bypassed,
            "claude_sources": [
                "src/utils/hooks/execHttpHook.ts envProxyActive",
                "src/utils/proxy.ts getProxyUrl",
                "src/utils/proxy.ts shouldBypassProxy"
            ]
        })
    }
}

pub(crate) fn webhook_proxy_policy(url: &str) -> WebhookProxyPolicy {
    let proxy =
        first_webhook_env_value(&["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"]);
    let no_proxy = first_webhook_env_value(&["no_proxy", "NO_PROXY"]);
    let bypassed = proxy.is_some()
        && webhook_should_bypass_proxy(url, no_proxy.as_ref().map(|(value, _)| value.as_str()));
    WebhookProxyPolicy {
        proxy_url: if bypassed {
            None
        } else {
            proxy.as_ref().map(|(value, _)| value.clone())
        },
        proxy_source: proxy.as_ref().map(|(_, source)| *source),
        no_proxy_source: no_proxy.as_ref().map(|(_, source)| *source),
        bypassed,
    }
}

#[cfg(test)]
pub(crate) fn webhook_proxy_policy_for_env(
    url: &str,
    env_map: &std::collections::BTreeMap<String, String>,
) -> WebhookProxyPolicy {
    let proxy = first_webhook_env_value_from_map(
        env_map,
        &["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"],
    );
    let no_proxy = first_webhook_env_value_from_map(env_map, &["no_proxy", "NO_PROXY"]);
    let bypassed = proxy.is_some()
        && webhook_should_bypass_proxy(url, no_proxy.as_ref().map(|(value, _)| value.as_str()));
    WebhookProxyPolicy {
        proxy_url: if bypassed {
            None
        } else {
            proxy.as_ref().map(|(value, _)| value.clone())
        },
        proxy_source: proxy.as_ref().map(|(_, source)| *source),
        no_proxy_source: no_proxy.as_ref().map(|(_, source)| *source),
        bypassed,
    }
}

fn first_webhook_env_value(candidates: &[&'static str]) -> Option<(String, &'static str)> {
    for name in candidates {
        let Some(value) = env::var_os(name).and_then(|value| value.into_string().ok()) else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() {
            return Some((value.to_owned(), *name));
        }
    }
    None
}

#[cfg(test)]
fn first_webhook_env_value_from_map(
    env_map: &std::collections::BTreeMap<String, String>,
    candidates: &[&'static str],
) -> Option<(String, &'static str)> {
    for name in candidates {
        let Some(value) = env_map.get(*name).map(|value| value.trim()) else {
            continue;
        };
        if !value.is_empty() {
            return Some((value.to_owned(), *name));
        }
    }
    None
}

pub(crate) fn webhook_should_bypass_proxy(url: &str, no_proxy: Option<&str>) -> bool {
    let Some(no_proxy) = no_proxy.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if no_proxy == "*" {
        return true;
    }
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(hostname) = parsed.host_str().map(|host| host.to_ascii_lowercase()) else {
        return false;
    };
    let port = parsed
        .port_or_known_default()
        .map(|port| port.to_string())
        .unwrap_or_default();
    let host_with_port = format!("{hostname}:{port}");
    no_proxy
        .split(|ch: char| ch == ',' || ch.is_ascii_whitespace())
        .filter_map(|pattern| {
            let pattern = pattern.trim().to_ascii_lowercase();
            (!pattern.is_empty()).then_some(pattern)
        })
        .any(|pattern| {
            if pattern.contains(':') {
                return host_with_port == pattern;
            }
            if let Some(suffix) = pattern.strip_prefix('.') {
                return hostname == suffix || hostname.ends_with(&format!(".{suffix}"));
            }
            hostname == pattern
        })
}

pub(crate) fn webhook_ssrf_report(
    ssrf_guard: Option<&WebhookSsrfGuard>,
    proxy_policy: &WebhookProxyPolicy,
) -> Value {
    if let Some(ssrf_guard) = ssrf_guard {
        return ssrf_guard.report();
    }
    json!({
        "mode": "skipped_proxy",
        "reason": "proxy_handles_target_dns",
        "socket_bound": false,
        "proxy_source": proxy_policy.proxy_source,
        "claude_sources": [
            "src/utils/hooks/execHttpHook.ts lookup",
            "src/utils/hooks/execHttpHook.ts envProxyActive"
        ]
    })
}

#[derive(Clone, Default)]
pub(crate) struct WebhookSsrfResolver {
    resolved_addresses: Arc<Mutex<BTreeMap<String, BTreeSet<String>>>>,
}

impl WebhookSsrfResolver {
    fn record_resolved_addresses(&self, hostname: &str, addrs: &[SocketAddr]) {
        let mut resolved = self.resolved_addresses.lock().unwrap();
        let host_resolutions = resolved.entry(hostname.to_owned()).or_default();
        for addr in addrs {
            host_resolutions.insert(addr.ip().to_string());
        }
    }

    fn report_for_host(&self, host: &str) -> Value {
        let resolved = self.resolved_addresses.lock().unwrap();
        let addresses = resolved
            .get(host)
            .map(|addresses| addresses.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        json!({
            "mode": "reqwest_dns_resolver",
            "host": host,
            "resolved_addresses": addresses,
            "socket_bound": true
        })
    }
}

impl reqwest::dns::Resolve for WebhookSsrfResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let hostname = name.as_str().to_owned();
        let resolver = self.clone();
        Box::pin(async move {
            let addrs = if let Ok(ip) = hostname.parse::<IpAddr>() {
                vec![SocketAddr::new(ip, 0)]
            } else {
                tokio::net::lookup_host((hostname.as_str(), 0))
                    .await
                    .map_err(|error| {
                        webhook_ssrf_box_error(format!(
                            "webhook hook blocked: DNS lookup failed for {hostname}: {error}"
                        ))
                    })?
                    .collect::<Vec<_>>()
            };
            let addrs = validate_webhook_resolved_socket_addrs(&hostname, addrs)
                .map_err(webhook_ssrf_box_error)?;
            resolver.record_resolved_addresses(&hostname, &addrs);
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub(crate) struct WebhookSsrfGuard {
    host: String,
    ip_literal: Option<IpAddr>,
    resolver: WebhookSsrfResolver,
}

impl WebhookSsrfGuard {
    pub(crate) fn new(url: &str) -> Result<Self, String> {
        let parsed = reqwest::Url::parse(url).map_err(|error| {
            format!("webhook hook blocked: invalid URL for SSRF validation: {error}")
        })?;
        let Some(host) = parsed.host_str().map(str::to_owned) else {
            return Err("webhook hook blocked: URL host is required".to_owned());
        };
        let ip_literal = match host.parse::<IpAddr>() {
            Ok(ip) => {
                validate_webhook_ip_address(&host, ip)?;
                Some(ip)
            }
            Err(_) => None,
        };
        Ok(Self {
            host,
            ip_literal,
            resolver: WebhookSsrfResolver::default(),
        })
    }

    pub(crate) fn resolver(&self) -> WebhookSsrfResolver {
        self.resolver.clone()
    }

    fn report(&self) -> Value {
        if let Some(ip) = self.ip_literal {
            return json!({
                "mode": "ip_literal",
                "host": self.host,
                "resolved_addresses": [ip.to_string()],
                "socket_bound": true
            });
        }
        self.resolver.report_for_host(&self.host)
    }
}

pub(crate) fn validate_webhook_resolved_socket_addrs(
    hostname: &str,
    addrs: Vec<SocketAddr>,
) -> Result<Vec<SocketAddr>, String> {
    if addrs.is_empty() {
        return Err(format!(
            "webhook hook blocked: DNS lookup returned no addresses for {hostname}"
        ));
    }
    for addr in &addrs {
        validate_webhook_ip_address(hostname, addr.ip())?;
    }
    Ok(addrs)
}

fn webhook_ssrf_box_error(error: String) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(error))
}

fn validate_webhook_ip_address(hostname: &str, ip: IpAddr) -> Result<(), String> {
    if is_blocked_webhook_ip(ip) {
        return Err(format!(
            "webhook hook blocked: {hostname} resolves to {ip} (private/link-local address). Loopback (127.0.0.1, ::1) is allowed for local dev."
        ));
    }
    Ok(())
}

fn is_blocked_webhook_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_blocked_webhook_ipv4(ip),
        IpAddr::V6(ip) => is_blocked_webhook_ipv6(ip),
    }
}

fn is_blocked_webhook_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    if a == 127 {
        return false;
    }
    if a == 0 || a == 10 {
        return true;
    }
    if a == 100 && (64..=127).contains(&b) {
        return true;
    }
    if a == 169 && b == 254 {
        return true;
    }
    if a == 172 && (16..=31).contains(&b) {
        return true;
    }
    if a == 192 && b == 168 {
        return true;
    }
    false
}

fn is_blocked_webhook_ipv6(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() {
        return false;
    }
    if ip.is_unspecified() {
        return true;
    }
    if let Some(mapped) = ipv6_mapped_ipv4(ip) {
        return is_blocked_webhook_ipv4(mapped);
    }
    let first = ip.segments()[0];
    if (first & 0xfe00) == 0xfc00 {
        return true;
    }
    if (first & 0xffc0) == 0xfe80 {
        return true;
    }
    false
}

fn ipv6_mapped_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    let segments = ip.segments();
    if segments[0] == 0
        && segments[1] == 0
        && segments[2] == 0
        && segments[3] == 0
        && segments[4] == 0
        && segments[5] == 0xffff
    {
        let hi = segments[6];
        let lo = segments[7];
        return Some(Ipv4Addr::new(
            (hi >> 8) as u8,
            (hi & 0xff) as u8,
            (lo >> 8) as u8,
            (lo & 0xff) as u8,
        ));
    }
    None
}

pub(crate) fn effective_webhook_allowed_env_vars(
    hook_allowed_env_vars: &[String],
    policy_allowed_env_vars: Option<&[String]>,
) -> Vec<String> {
    match policy_allowed_env_vars {
        Some(policy_allowed_env_vars) => hook_allowed_env_vars
            .iter()
            .filter(|name| {
                policy_allowed_env_vars
                    .iter()
                    .any(|allowed| allowed == *name)
            })
            .cloned()
            .collect(),
        None => hook_allowed_env_vars.to_vec(),
    }
}

pub(crate) fn webhook_url_policy_summary(allowed_urls: Option<&[String]>) -> Value {
    match allowed_urls {
        Some(patterns) => json!({
            "configured": true,
            "pattern_count": patterns.len()
        }),
        None => json!({
            "configured": false
        }),
    }
}

pub(crate) fn interpolate_webhook_header_value(value: &str, allowed_env_vars: &[String]) -> String {
    let mut output = String::new();
    let bytes = value.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            output.push(bytes[index] as char);
            index += 1;
            continue;
        }
        if index + 1 < bytes.len() && bytes[index + 1] == b'{' {
            if let Some(end) = value[index + 2..].find('}') {
                let name = &value[index + 2..index + 2 + end];
                output.push_str(&allowed_webhook_env_value(name, allowed_env_vars));
                index += end + 3;
                continue;
            }
        }
        let start = index + 1;
        if start < bytes.len() && is_webhook_env_name_start(bytes[start]) {
            let mut end = start + 1;
            while end < bytes.len() && is_webhook_env_name_char(bytes[end]) {
                end += 1;
            }
            let name = &value[start..end];
            output.push_str(&allowed_webhook_env_value(name, allowed_env_vars));
            index = end;
            continue;
        }
        output.push('$');
        index += 1;
    }
    output
        .chars()
        .filter(|character| !matches!(character, '\r' | '\n' | '\0'))
        .collect()
}

fn allowed_webhook_env_value(name: &str, allowed_env_vars: &[String]) -> String {
    if allowed_env_vars.iter().any(|allowed| allowed == name) {
        env::var(name).unwrap_or_default()
    } else {
        String::new()
    }
}

fn is_webhook_env_name_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_uppercase()
}

fn is_webhook_env_name_char(byte: u8) -> bool {
    is_webhook_env_name_start(byte) || byte.is_ascii_digit()
}
