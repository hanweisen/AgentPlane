use axum::Json;
use axum::http::StatusCode;

use crate::protocol::SimpleResponse;

pub(super) fn unauthorized_response() -> (StatusCode, Json<SimpleResponse>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(SimpleResponse {
            ok: false,
            error: Some("unauthorized".to_string()),
        }),
    )
}

pub(super) fn bad_request_response(error: anyhow::Error) -> (StatusCode, Json<SimpleResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(SimpleResponse {
            ok: false,
            error: Some(error.to_string()),
        }),
    )
}
