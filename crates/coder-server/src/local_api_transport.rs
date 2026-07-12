use std::{io, net::SocketAddr};

use axum::http::{header, HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

pub(crate) fn validate_bind_address(addr: SocketAddr) -> io::Result<()> {
    if addr.ip().is_loopback() {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("refusing to start unauthenticated Coder API on non-loopback address {addr}"),
    ))
}

pub(crate) fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin, _| {
            local_frontend_origin(origin)
        }))
        .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE])
}

fn local_frontend_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    if matches!(
        origin,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
    ) {
        return true;
    }
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
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
    fn api_listener_requires_loopback() {
        assert!(validate_bind_address("127.0.0.1:8876".parse().unwrap()).is_ok());
        assert!(validate_bind_address("[::1]:8876".parse().unwrap()).is_ok());
        assert!(validate_bind_address("0.0.0.0:8876".parse().unwrap()).is_err());
    }

    #[test]
    fn cors_accepts_only_local_frontend_origins() {
        for origin in [
            "http://127.0.0.1:5173",
            "http://localhost:5173",
            "http://[::1]:5173",
            "tauri://localhost",
        ] {
            assert!(local_frontend_origin(
                &HeaderValue::from_str(origin).unwrap()
            ));
        }
        for origin in [
            "https://example.com",
            "https://localhost.example.com",
            "file://localhost/app.html",
        ] {
            assert!(!local_frontend_origin(
                &HeaderValue::from_str(origin).unwrap()
            ));
        }
    }
}
