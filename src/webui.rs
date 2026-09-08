use crate::{api::ApiResponseError, config::Config};
use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

pub fn authorize(config: &Config, headers: &HeaderMap) -> Result<(), ApiResponseError> {
    if authorized(config, headers, Access::Admin) {
        Ok(())
    } else {
        Err(ApiResponseError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Enter the ZebraTamer admin token",
        ))
    }
}

#[derive(Clone, Copy)]
pub enum Access {
    Read,
    Print,
    Admin,
}

pub fn authorized(config: &Config, headers: &HeaderMap, access: Access) -> bool {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(supplied) = supplied else {
        return false;
    };
    let candidates = match access {
        Access::Read => [
            config.read_token.as_deref(),
            config.print_token.as_deref(),
            config.admin_token.as_deref(),
        ],
        Access::Print => [
            config.print_token.as_deref(),
            config.admin_token.as_deref(),
            None,
        ],
        Access::Admin => [config.admin_token.as_deref(), None, None],
    };
    candidates.into_iter().flatten().any(|expected| {
        expected.len() >= 24 && constant_time_equal(expected.as_bytes(), supplied.as_bytes())
    })
}

fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0, |difference, (a, b)| difference | (a ^ b))
        == 0
}
fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-store"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::CONTENT_SECURITY_POLICY, "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")], body).into_response()
}
pub async fn index() -> Response {
    asset("text/html; charset=utf-8", include_str!("webui.html"))
}
pub async fn javascript() -> Response {
    asset("text/javascript; charset=utf-8", include_str!("webui.js"))
}
pub async fn stylesheet() -> Response {
    asset("text/css; charset=utf-8", include_str!("webui.css"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admin_token_is_required_and_not_in_ui_assets() {
        let mut config = Config::default();
        let mut headers = HeaderMap::new();
        assert!(authorize(&config, &headers).is_err());
        config.admin_token = Some("a-long-test-token-0123456789".into());
        assert!(authorize(&config, &headers).is_err());
        headers.insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
        assert!(authorize(&config, &headers).is_err());
        headers.insert(
            header::AUTHORIZATION,
            "Bearer a-long-test-token-0123456789".parse().unwrap(),
        );
        assert!(authorize(&config, &headers).is_ok());
        config.webui_enabled = true;
        assert!(config.validate().is_ok());
        config.admin_token = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn v2_tokens_have_separate_scopes_and_admin_grants_every_scope() {
        let config = Config {
            read_token: Some("read-token-0123456789012345".into()),
            print_token: Some("print-token-012345678901234".into()),
            admin_token: Some("admin-token-012345678901234".into()),
            ..Config::default()
        };
        let headers = |token: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::AUTHORIZATION,
                format!("Bearer {token}").parse().unwrap(),
            );
            headers
        };
        let read = headers("read-token-0123456789012345");
        assert!(authorized(&config, &read, Access::Read));
        assert!(!authorized(&config, &read, Access::Print));
        let print = headers("print-token-012345678901234");
        assert!(authorized(&config, &print, Access::Read));
        assert!(authorized(&config, &print, Access::Print));
        assert!(!authorized(&config, &print, Access::Admin));
        let admin = headers("admin-token-012345678901234");
        assert!(authorized(&config, &admin, Access::Read));
        assert!(authorized(&config, &admin, Access::Print));
        assert!(authorized(&config, &admin, Access::Admin));
    }
}
