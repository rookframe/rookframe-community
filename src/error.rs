use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

pub struct ApiError(pub StatusCode, pub &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(_: sqlx::Error) -> Self {
        // Database diagnostics can contain private query values. Do not log them.
        tracing::error!("directory_storage_unavailable");
        Self(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable")
    }
}
