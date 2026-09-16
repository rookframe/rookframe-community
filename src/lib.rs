mod directory;
mod error;
mod moderation;
mod requests;
mod setup;
mod turn;
pub use requests::maintain as maintain_requests;
pub use turn::TurnProvider;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    routing::get,
};
use serde_json::{Value, json};
use sqlx::PgPool;

pub fn router(db: PgPool) -> Router {
    router_with_turn(db, TurnProvider::disabled())
}

pub fn router_with_turn(db: PgPool, turn: TurnProvider) -> Router {
    let operator = std::env::var("MODERATION_TOKEN_SHA256")
        .ok()
        .filter(|s| requests::secret(s))
        .and_then(|s| hex::decode(s).ok());
    router_with_moderation(db, turn, operator)
}

pub fn router_with_moderation(db: PgPool, turn: TurnProvider, operator: Option<Vec<u8>>) -> Router {
    let health_turn = turn.clone();
    let metrics_turn = turn.clone();
    Router::new()
        .route(
            "/api/v1/health",
            get(move |State(db): State<PgPool>| {
                let turn = health_turn.clone();
                async move {
                    let result = health(State(db)).await?;
                    if !turn.ready() {
                        return Err(turn::unavailable());
                    }
                    Ok(result)
                }
            }),
        )
        .route(
            "/metrics",
            get(move || {
                let metrics = metrics_turn.metrics();
                async move { ([("content-type", "text/plain; version=0.0.4")], metrics) }
            }),
        )
        .route(
            "/api/v1/live",
            get(|| async { Json(json!({"status":"ok"})) }),
        )
        .route("/api/v1/worlds", get(directory::browse))
        .route(
            "/api/v1/worlds/{world}/{address}",
            get(directory::read).put(directory::publish),
        )
        .route(
            "/api/v1/worlds/{world}/{address}/capacity",
            axum::routing::put(directory::capacity),
        )
        .with_state(db.clone())
        .merge(requests::router(db.clone()))
        .merge(moderation::router(db.clone(), operator))
        .merge(setup::router(db, turn))
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
}

async fn health(State(db): State<PgPool>) -> Result<Json<Value>, error::ApiError> {
    sqlx::query("SELECT revision,admission_revision,reserved_seats,claimed_seats FROM world_addresses LIMIT 1")
        .execute(&db)
        .await?;
    Ok(Json(json!({"status":"ok","api_version":1})))
}
