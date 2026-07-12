use std::{env, fmt, fs, path::PathBuf};

use reqwest::{Client, ClientBuilder, Proxy};
use serde_json::{json, Value};

const CODER_CA_CERT_ENV: &str = "CODER_CA_CERTIFICATE";
const SSL_CERT_FILE_ENV: &str = "SSL_CERT_FILE";
const STANDARD_PROXY_ENV_KEYS: &[&str] =
    &["https_proxy", "HTTPS_PROXY", "http_proxy", "HTTP_PROXY"];
const NO_PROXY_ENV_KEYS: &[&str] = &["no_proxy", "NO_PROXY"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClientRouteClass {
    ProviderApi,
    Webhook,
}

impl fmt::Display for ClientRouteClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ProviderApi => "provider_api",
            Self::Webhook => "webhook",
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum OutboundProxyRoute {
    TransportDefault,
    Direct,
    Proxy { url: String },
}

impl fmt::Debug for OutboundProxyRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TransportDefault => formatter.write_str("TransportDefault"),
            Self::Direct => formatter.write_str("Direct"),
            Self::Proxy { .. } => formatter
                .debug_struct("Proxy")
                .field("url", &"<redacted>")
                .finish(),
        }
    }
}

impl OutboundProxyRoute {
    pub(crate) fn from_optional_url(proxy_url: Option<&str>) -> Self {
        match proxy_url.map(str::trim).filter(|url| !url.is_empty()) {
            Some(url) => Self::Proxy {
                url: url.to_owned(),
            },
            None => Self::Direct,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvironmentProxyRoute {
    pub(crate) proxy_url: Option<String>,
    pub(crate) proxy_source: Option<String>,
    no_proxy_source: Option<String>,
    bypassed: bool,
}

impl EnvironmentProxyRoute {
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
            "proxy_source": self.proxy_source.as_deref(),
            "no_proxy_configured": self.no_proxy_source.is_some(),
            "no_proxy_source": self.no_proxy_source.as_deref(),
            "proxy_bypassed": self.bypassed
        })
    }

    pub(crate) fn outbound_route(&self) -> OutboundProxyRoute {
        if self.bypassed {
            return OutboundProxyRoute::Direct;
        }
        self.proxy_url
            .as_deref()
            .map(|url| OutboundProxyRoute::Proxy {
                url: url.to_owned(),
            })
            .unwrap_or(OutboundProxyRoute::TransportDefault)
    }
}

pub(crate) fn environment_proxy_route(
    request_url: &str,
    preferred_proxy_env_keys: &[String],
) -> EnvironmentProxyRoute {
    environment_proxy_route_with(request_url, preferred_proxy_env_keys, |name| {
        env::var(name).ok()
    })
}

#[cfg(test)]
pub(crate) fn environment_proxy_route_from_map(
    request_url: &str,
    preferred_proxy_env_keys: &[String],
    values: &std::collections::BTreeMap<String, String>,
) -> EnvironmentProxyRoute {
    environment_proxy_route_with(request_url, preferred_proxy_env_keys, |name| {
        values.get(name).cloned()
    })
}

fn environment_proxy_route_with(
    request_url: &str,
    preferred_proxy_env_keys: &[String],
    get_env: impl Fn(&str) -> Option<String>,
) -> EnvironmentProxyRoute {
    let proxy = preferred_proxy_env_keys
        .iter()
        .map(String::as_str)
        .chain(STANDARD_PROXY_ENV_KEYS.iter().copied())
        .find_map(|name| non_empty_env_value(name, &get_env));
    let no_proxy = NO_PROXY_ENV_KEYS
        .iter()
        .find_map(|name| non_empty_env_value(name, &get_env));
    let bypassed = proxy.is_some()
        && url_matches_no_proxy(
            request_url,
            no_proxy.as_ref().map(|(value, _)| value.as_str()),
        );
    EnvironmentProxyRoute {
        proxy_url: if bypassed {
            None
        } else {
            proxy.as_ref().map(|(value, _)| value.clone())
        },
        proxy_source: proxy.map(|(_, source)| source),
        no_proxy_source: no_proxy.map(|(_, source)| source),
        bypassed,
    }
}

fn non_empty_env_value(
    name: &str,
    get_env: &impl Fn(&str) -> Option<String>,
) -> Option<(String, String)> {
    get_env(name)
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(|value| (value, name.to_owned()))
}

pub(crate) fn url_matches_no_proxy(url: &str, no_proxy: Option<&str>) -> bool {
    let Some(no_proxy) = no_proxy.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if no_proxy == "*" {
        return true;
    }
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let hostname = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    let port = parsed.port_or_known_default().map(|port| port.to_string());
    let host_with_port = port
        .as_deref()
        .map(|port| format!("{hostname}:{port}"))
        .unwrap_or_else(|| hostname.clone());
    no_proxy
        .split(|character: char| character == ',' || character.is_ascii_whitespace())
        .filter_map(|entry| {
            let entry = entry.trim().to_ascii_lowercase();
            (!entry.is_empty()).then_some(entry)
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

#[derive(Debug, Clone)]
pub(crate) struct HttpClientFactory {
    route: OutboundProxyRoute,
}

impl HttpClientFactory {
    pub(crate) fn new(route: OutboundProxyRoute) -> Self {
        Self { route }
    }

    pub(crate) fn builder(
        &self,
        request_url: &str,
        route_class: ClientRouteClass,
    ) -> Result<ClientBuilder, String> {
        self.configure(Client::builder(), request_url, route_class)
    }

    pub(crate) fn configure(
        &self,
        mut builder: ClientBuilder,
        request_url: &str,
        route_class: ClientRouteClass,
    ) -> Result<ClientBuilder, String> {
        builder = if request_url_is_loopback(request_url) {
            builder.no_proxy()
        } else {
            match &self.route {
                OutboundProxyRoute::TransportDefault => builder,
                OutboundProxyRoute::Direct => builder.no_proxy(),
                OutboundProxyRoute::Proxy { url } => {
                    let proxy = Proxy::all(url).map_err(|_| {
                        format!("outbound proxy configuration is invalid for {route_class}")
                    })?;
                    builder.proxy(proxy)
                }
            }
        };
        configure_custom_ca(builder)
    }
}

fn configure_custom_ca(mut builder: ClientBuilder) -> Result<ClientBuilder, String> {
    let Some(bundle) = configured_ca_bundle(|name| env::var(name).ok()) else {
        return Ok(builder);
    };
    let pem = fs::read(&bundle.path).map_err(|error| {
        format!(
            "failed to read CA certificate file {} selected by {}: {error}",
            bundle.path.display(),
            bundle.source_env
        )
    })?;
    let certificates = reqwest::Certificate::from_pem_bundle(&pem).map_err(|error| {
        format!(
            "failed to load CA certificates from {} selected by {}: {error}",
            bundle.path.display(),
            bundle.source_env
        )
    })?;
    if certificates.is_empty() {
        return Err(format!(
            "CA certificate file {} selected by {} contains no certificates",
            bundle.path.display(),
            bundle.source_env
        ));
    }
    for certificate in certificates {
        builder = builder.add_root_certificate(certificate);
    }
    Ok(builder)
}

#[derive(Debug, PartialEq, Eq)]
struct ConfiguredCaBundle {
    source_env: &'static str,
    path: PathBuf,
}

fn configured_ca_bundle(get_env: impl Fn(&str) -> Option<String>) -> Option<ConfiguredCaBundle> {
    [CODER_CA_CERT_ENV, SSL_CERT_FILE_ENV]
        .into_iter()
        .find_map(|source_env| {
            get_env(source_env)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .map(|path| ConfiguredCaBundle {
                    source_env,
                    path: PathBuf::from(path),
                })
        })
}

fn request_url_is_loopback(request_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(request_url) else {
        return false;
    };
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_route_debug_output_redacts_credentials() {
        let route = OutboundProxyRoute::Proxy {
            url: "http://user:secret@127.0.0.1:7890".to_owned(),
        };
        let debug = format!("{route:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn invalid_proxy_error_reports_only_route_class() {
        let error = HttpClientFactory::new(OutboundProxyRoute::Proxy {
            url: "://secret.invalid".to_owned(),
        })
        .builder("https://api.example.invalid", ClientRouteClass::ProviderApi)
        .unwrap_err();
        assert_eq!(
            error,
            "outbound proxy configuration is invalid for provider_api"
        );
        assert!(!error.contains("secret.invalid"));
    }

    #[test]
    fn loopback_route_bypasses_explicit_proxy() {
        let _ = HttpClientFactory::new(OutboundProxyRoute::Proxy {
            url: "://invalid".to_owned(),
        })
        .builder("http://127.0.0.1:8876", ClientRouteClass::ProviderApi)
        .unwrap();
    }

    #[test]
    fn route_classes_cover_active_external_network_boundaries() {
        assert_eq!(ClientRouteClass::ProviderApi.to_string(), "provider_api");
        assert_eq!(ClientRouteClass::Webhook.to_string(), "webhook");
    }

    #[test]
    fn coder_ca_precedes_ssl_cert_file_and_empty_values_are_ignored() {
        let values = std::collections::BTreeMap::from([
            (CODER_CA_CERT_ENV, " C:/coder-ca.pem "),
            (SSL_CERT_FILE_ENV, "C:/ssl-ca.pem"),
        ]);
        assert_eq!(
            configured_ca_bundle(|name| values.get(name).map(ToString::to_string)),
            Some(ConfiguredCaBundle {
                source_env: CODER_CA_CERT_ENV,
                path: PathBuf::from("C:/coder-ca.pem"),
            })
        );

        let values = std::collections::BTreeMap::from([
            (CODER_CA_CERT_ENV, " "),
            (SSL_CERT_FILE_ENV, "C:/ssl-ca.pem"),
        ]);
        assert_eq!(
            configured_ca_bundle(|name| values.get(name).map(ToString::to_string)),
            Some(ConfiguredCaBundle {
                source_env: SSL_CERT_FILE_ENV,
                path: PathBuf::from("C:/ssl-ca.pem"),
            })
        );
    }

    #[test]
    fn environment_proxy_route_uses_preferred_source_and_no_proxy() {
        let values = std::collections::BTreeMap::from([
            (
                "CODER_DEEPSEEK_PROXY_URL".to_owned(),
                "http://provider-proxy:8080".to_owned(),
            ),
            (
                "HTTPS_PROXY".to_owned(),
                "http://general-proxy:8080".to_owned(),
            ),
            ("NO_PROXY".to_owned(), ".internal.example".to_owned()),
        ]);
        let preferred = vec!["CODER_DEEPSEEK_PROXY_URL".to_owned()];
        let proxied =
            environment_proxy_route_from_map("https://api.example.com/v1", &preferred, &values);
        assert_eq!(
            proxied.proxy_url.as_deref(),
            Some("http://provider-proxy:8080")
        );
        assert_eq!(
            proxied.proxy_source.as_deref(),
            Some("CODER_DEEPSEEK_PROXY_URL")
        );

        let bypassed = environment_proxy_route_from_map(
            "https://api.internal.example/v1",
            &preferred,
            &values,
        );
        assert!(!bypassed.uses_proxy());
        assert_eq!(bypassed.report()["mode"], "env_proxy_bypassed");
    }
}
