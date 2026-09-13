mod directory;
mod error;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    routing::get,
};
use serde_json::{Value, json};
use sqlx::PgPool;

pub fn router(db: PgPool) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route(
            "/api/v1/live",
            get(|| async { Json(json!({"status":"ok"})) }),
        )
        .route("/api/v1/worlds", get(directory::browse))
        .route(
            "/api/v1/worlds/{world}/{address}",
            get(directory::read).put(directory::publish),
        )
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(
            tower::ServiceBuilder::new()
                .layer(axum::error_handling::HandleErrorLayer::new(
                    |_: axum::BoxError| async {
                        error::ApiError(axum::http::StatusCode::SERVICE_UNAVAILABLE, "service_busy")
                    },
                ))
                .load_shed()
                .concurrency_limit(64)
                .timeout(std::time::Duration::from_secs(15)),
        )
        .with_state(db)
}

async fn health(State(db): State<PgPool>) -> Result<Json<Value>, error::ApiError> {
    sqlx::query("SELECT revision FROM world_addresses LIMIT 1")
        .execute(&db)
        .await?;
    Ok(Json(json!({"status":"ok","api_version":1})))
}
