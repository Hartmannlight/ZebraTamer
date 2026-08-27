use crate::{api::ApiResponseError, config::Config};
use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

pub fn authorize(config: &Config, headers: &HeaderMap) -> Result<(), ApiResponseError> {
    let expected = config
        .admin_token
        .as_deref()
        .filter(|token| token.len() >= 24);
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match (expected, supplied) {
        (Some(expected), Some(supplied))
            if constant_time_equal(expected.as_bytes(), supplied.as_bytes()) =>
        {
            Ok(())
        }
        _ => Err(ApiResponseError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Enter the ZebraTamer admin token",
        )),
    }
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
}
