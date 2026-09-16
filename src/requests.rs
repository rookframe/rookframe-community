use crate::error::ApiError;
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction, postgres::PgRow};
use std::net::SocketAddr;
use uuid::Uuid;

type Target = (Uuid, Uuid);
type RequestTarget = (Uuid, Uuid, Uuid);

pub fn router(db: PgPool) -> Router {
    Router::new()
        .route("/api/v1/worlds/{world}/{address}/requests", get(review))
        .route(
            "/api/v1/worlds/{world}/{address}/requests/{request}",
            get(read).put(submit),
        )
        .route(
            "/api/v1/worlds/{world}/{address}/requests/{request}/withdraw",
            axum::routing::post(withdraw),
        )
        .route(
            "/api/v1/worlds/{world}/{address}/requests/{request}/decision",
            axum::routing::put(decide),
        )
        .with_state(db)
        .layer(axum::middleware::from_fn(
            |request: axum::extract::Request, next: axum::middleware::Next| async move {
                let mut response = next.run(request).await;
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
}

#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    name: String,
    message: String,
}

pub(crate) fn identity(headers: &HeaderMap) -> Result<Vec<u8>, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| secret(v))
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "credential_required"))?;
    Ok(Sha256::digest(token.as_bytes()).to_vec())
}
pub(crate) fn secret(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
pub(crate) fn valid_text(value: &str, maximum: usize, multiline: bool) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.encode_utf16().count() <= maximum
        && !value
            .chars()
            .any(|c| c.is_control() && !(multiline && matches!(c, '\n' | '\r')))
}
pub(crate) fn valid_id(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}
pub(crate) async fn lock_world(
    tx: &mut Transaction<'_, Postgres>,
    (world, address): Target,
) -> Result<PgRow, ApiError> {
    sqlx::query("SELECT * FROM world_addresses WHERE world_id=$1 AND world_address=$2 FOR UPDATE")
        .bind(world)
        .bind(address)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "listing_not_found"))
}
pub(crate) fn administrator(row: &PgRow, digest: &[u8]) -> Result<(), ApiError> {
    if row.get::<Vec<u8>, _>("administrator_digest") != digest {
        return Err(ApiError(StatusCode::FORBIDDEN, "administrator_mismatch"));
    }
    Ok(())
}

// Expired decisions retain their operation proof for the terminal retention
// window, allowing an already durable World receipt to finish reconciliation.
pub async fn cleanup(db: &PgPool) -> Result<(), ApiError> {
    sqlx::query("UPDATE join_requests SET status='expired',terminal_at=expires_at WHERE status='pending' AND expires_at<=now()")
        .execute(db).await?;
    sqlx::query("UPDATE join_requests SET name=NULL,message=NULL WHERE expires_at<=now() AND (name IS NOT NULL OR message IS NOT NULL)")
        .execute(db).await?;
    sqlx::query("DELETE FROM join_requests WHERE terminal_at<=now()-interval '30 days'")
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM request_rate_limits WHERE bucket<now()-interval '2 hours'")
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM abuse_reports WHERE created_at<=now()-interval '30 days'")
        .execute(db)
        .await?;
    sqlx::query("DELETE FROM player_removals WHERE created_at<=now()-interval '30 days'")
        .execute(db)
        .await?;
    Ok(())
}

pub(crate) async fn rate(
    db: &PgPool,
    digest: &[u8],
    network: Option<SocketAddr>,
    headers: &HeaderMap,
    submission: bool,
) -> Result<(), ApiError> {
    // Caddy overwrites this header; only an explicitly configured private proxy
    // listener trusts it. Standalone servers use the actual TCP peer address.
    let ip = if std::env::var("TRUST_PROXY").as_deref() == Ok("true") {
        headers
            .get("x-rookframe-client-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<std::net::IpAddr>().ok())
    } else {
        network.map(|n| n.ip())
    }
    .ok_or(ApiError(StatusCode::BAD_REQUEST, "network_required"))?;
    let network = Sha256::digest(ip.to_string().as_bytes()).to_vec();
    let mut tx = db.begin().await?;
    for (scope, key, maximum) in if submission {
        [
            ("submit_installation", digest, 10),
            ("submit_network", network.as_slice(), 100),
        ]
    } else {
        [
            ("check_installation", digest, 600),
            ("check_network", network.as_slice(), 6000),
        ]
    } {
        let hits: i32 = sqlx::query_scalar("INSERT INTO request_rate_limits(scope,identity_digest,bucket,hits) VALUES($1,$2,date_trunc('hour',now()),1) ON CONFLICT(scope,identity_digest,bucket) DO UPDATE SET hits=request_rate_limits.hits+1 RETURNING hits")
            .bind(scope).bind(key).fetch_one(&mut *tx).await?;
        if hits > maximum {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "request_rate_limited",
            ));
        }
    }
    tx.commit().await?;
    Ok(())
}

fn view(row: &PgRow, gm: bool) -> Value {
    let mut value = json!({"request_id":row.get::<Uuid,_>("request_id"),
        "name":row.get::<Option<String>,_>("name"),"message":row.get::<Option<String>,_>("message"),
        "status":row.get::<String,_>("status"),"response":row.get::<Option<String>,_>("response"),
        "created_at":row.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        "expires_at":row.get::<chrono::DateTime<chrono::Utc>,_>("expires_at"),
        "decision_id":row.get::<Option<Uuid>,_>("decision_id"), "removed":row.get::<bool,_>("removed"), "blocked":row.get::<bool,_>("blocked")});
    if gm {
        value["installation_hash"] =
            json!(hex::encode(row.get::<Vec<u8>, _>("installation_digest")));
    } else {
        value["receipt"] = row
            .get::<Option<Value>, _>("receipt")
            .unwrap_or(Value::Null);
    }
    value
}

pub async fn submit(
    State(db): State<PgPool>,
    Path((world, address, id)): Path<RequestTarget>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Submission>,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    if !valid_id(world)
        || !valid_id(address)
        || world == address
        || !valid_id(id)
        || !valid_text(&body.name, 100, false)
        || !valid_text(&body.message, 4000, true)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_join_request"));
    }
    let submission_digest = Sha256::digest(serde_json::to_vec(&body).expect("submission")).to_vec();
    cleanup(&db).await?;
    rate(&db, &digest, Some(network.0), &headers, false).await?;
    rate(&db, &digest, Some(network.0), &headers, true).await?;
    let mut tx = db.begin().await?;
    let listing = lock_world(&mut tx, (world, address)).await?;
    if let Some(row) = sqlx::query("SELECT * FROM join_requests WHERE request_id=$1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
    {
        if row.get::<Uuid, _>("world_id") != world
            || row.get::<Uuid, _>("world_address") != address
            || row.get::<Vec<u8>, _>("installation_digest") != digest
            || row.get::<Vec<u8>, _>("submission_digest") != submission_digest
        {
            return Err(ApiError(StatusCode::CONFLICT, "request_conflict"));
        }
        return Ok(Json(view(&row, false)));
    }
    if listing.get::<bool, _>("moderated")
        || listing.get::<Option<Value>, _>("listing").is_none()
        || listing.get::<chrono::DateTime<chrono::Utc>, _>("checked_in_at")
            <= chrono::Utc::now() - chrono::Duration::days(30)
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "listing_not_found"));
    }
    let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM installation_blocks WHERE world_id=$1 AND world_address=$2 AND installation_digest=$3)")
        .bind(world).bind(address).bind(&digest).fetch_one(&mut *tx).await?;
    if blocked {
        return Err(ApiError(StatusCode::FORBIDDEN, "installation_blocked"));
    }
    let listing_details: Value = listing.get("listing");
    if listing.get::<i32, _>("reserved_seats") as i64
        >= listing_details["player_limit"].as_i64().unwrap_or(0)
    {
        return Err(ApiError(StatusCode::CONFLICT, "world_full"));
    }
    let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM join_requests WHERE world_id=$1 AND world_address=$2 AND installation_digest=$3 AND status IN ('pending','accepted') AND NOT removed)")
        .bind(world).bind(address).bind(&digest).fetch_one(&mut *tx).await?;
    if active {
        return Err(ApiError(StatusCode::CONFLICT, "active_request_exists"));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM join_requests WHERE world_id=$1 AND world_address=$2 AND status='pending'")
        .bind(world).bind(address).fetch_one(&mut *tx).await?;
    if count >= 100 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "request_queue_full",
        ));
    }
    let row = sqlx::query("INSERT INTO join_requests(request_id,world_id,world_address,installation_digest,name,message,submission_digest) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *")
        .bind(id).bind(world).bind(address).bind(digest).bind(body.name).bind(body.message).bind(submission_digest).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(view(&row, false)))
}

pub async fn read(
    State(db): State<PgPool>,
    Path((world, address, id)): Path<RequestTarget>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    rate(&db, &digest, Some(network.0), &headers, false).await?;
    cleanup(&db).await?;
    let row = sqlx::query("SELECT * FROM join_requests WHERE request_id=$1 AND world_id=$2 AND world_address=$3 AND installation_digest=$4")
        .bind(id).bind(world).bind(address).bind(&digest).fetch_optional(&db).await?;
    if let Some(row) = row {
        return Ok(Json(view(&row, false)));
    }
    // The private read budget still applies. Recover a known block without a
    // write attempt, even when earlier submissions exhausted their own quota.
    let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM installation_blocks WHERE world_id=$1 AND world_address=$2 AND installation_digest=$3)")
        .bind(world).bind(address).bind(digest).fetch_one(&db).await?;
    Err(ApiError(
        if blocked {
            StatusCode::FORBIDDEN
        } else {
            StatusCode::NOT_FOUND
        },
        if blocked {
            "installation_blocked"
        } else {
            "request_not_found"
        },
    ))
}

pub async fn review(
    State(db): State<PgPool>,
    Path(target): Path<Target>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    cleanup(&db).await?;
    let mut tx = db.begin().await?;
    administrator(&lock_world(&mut tx, target).await?, &digest)?;
    let rows = sqlx::query("SELECT * FROM join_requests WHERE world_id=$1 AND world_address=$2 ORDER BY (status='pending') DESC,created_at DESC LIMIT 200")
        .bind(target.0).bind(target.1).fetch_all(&mut *tx).await?;
    Ok(Json(
        json!({"requests":rows.iter().map(|r| view(r,true)).collect::<Vec<_>>()}),
    ))
}

async fn locked_request(
    tx: &mut Transaction<'_, Postgres>,
    (world, address, id): RequestTarget,
) -> Result<PgRow, ApiError> {
    sqlx::query("SELECT * FROM join_requests WHERE world_id=$1 AND world_address=$2 AND request_id=$3 FOR UPDATE")
        .bind(world).bind(address).bind(id).fetch_optional(&mut **tx).await?
        .ok_or(ApiError(StatusCode::NOT_FOUND,"request_not_found"))
}

pub async fn withdraw(
    State(db): State<PgPool>,
    Path(target): Path<RequestTarget>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    rate(&db, &digest, Some(network.0), &headers, false).await?;
    cleanup(&db).await?;
    let mut tx = db.begin().await?;
    lock_world(&mut tx, (target.0, target.1)).await?;
    let row = locked_request(&mut tx, target).await?;
    if row.get::<Vec<u8>, _>("installation_digest") != digest {
        return Err(ApiError(StatusCode::NOT_FOUND, "request_not_found"));
    }
    if row.get::<String, _>("status") == "withdrawn" {
        return Ok(Json(view(&row, false)));
    }
    if row.get::<String, _>("status") != "pending"
        || row.get::<Option<Uuid>, _>("decision_id").is_some()
    {
        return Err(ApiError(StatusCode::CONFLICT, "decision_in_progress"));
    }
    let row = sqlx::query("UPDATE join_requests SET status='withdrawn',terminal_at=now() WHERE request_id=$1 RETURNING *")
        .bind(target.2).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(view(&row, false)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    decision_id: Uuid,
    action: String,
    receipt: Option<Receipt>,
    response: Option<String>,
}
#[derive(Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    seat_id: Uuid,
    name: String,
    credential: String,
}

pub async fn decide(
    State(db): State<PgPool>,
    Path(target): Path<RequestTarget>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Decision>,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    rate(&db, &digest, Some(network.0), &headers, false).await?;
    if !valid_id(body.decision_id)
        || !matches!(
            body.action.as_str(),
            "prepare" | "abort" | "accept" | "reject" | "reject-block"
        )
        || body
            .response
            .as_ref()
            .is_some_and(|s| !valid_text(s, 4000, true))
        || (body.action == "accept") != body.receipt.is_some()
        || !matches!(body.action.as_str(), "reject" | "reject-block") && body.response.is_some()
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_decision"));
    }
    if let Some(receipt) = &body.receipt
        && (receipt.seat_id != target.2
            || !valid_text(&receipt.name, 100, false)
            || !secret(&receipt.credential))
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_receipt"));
    }
    cleanup(&db).await?;
    let mut tx = db.begin().await?;
    administrator(&lock_world(&mut tx, (target.0, target.1)).await?, &digest)?;
    let row = locked_request(&mut tx, target).await?;
    let status: String = row.get("status");
    let previous: Option<Uuid> = row.get("decision_id");
    if status == "pending"
        && previous.is_none()
        && row.get::<chrono::DateTime<chrono::Utc>, _>("expires_at") <= chrono::Utc::now()
    {
        return Err(ApiError(StatusCode::CONFLICT, "request_terminal"));
    }
    let receipt = body
        .receipt
        .as_ref()
        .map(|r| serde_json::to_value(r).expect("receipt"));
    if status != "pending"
        && !(status == "expired" && previous == Some(body.decision_id) && body.action == "accept")
    {
        if previous == Some(body.decision_id)
            && ((status == "accepted"
                && body.action == "accept"
                && row.get::<Option<Value>, _>("receipt") == receipt)
                || (status == "rejected"
                    && matches!(body.action.as_str(), "reject" | "reject-block")
                    && row.get::<bool, _>("blocked") == (body.action == "reject-block")
                    && row.get::<Option<String>, _>("response") == body.response))
        {
            return Ok(Json(view(&row, true)));
        }
        return Err(ApiError(StatusCode::CONFLICT, "request_terminal"));
    }
    if previous.is_some_and(|id| id != body.decision_id) {
        return Err(ApiError(StatusCode::CONFLICT, "decision_in_progress"));
    }
    match body.action.as_str() {
        "prepare" => {
            sqlx::query("UPDATE join_requests SET decision_id=$2 WHERE request_id=$1")
                .bind(target.2)
                .bind(body.decision_id)
                .execute(&mut *tx)
                .await?;
        }
        "abort" => {
            if previous != Some(body.decision_id) {
                return Err(ApiError(StatusCode::CONFLICT, "decision_mismatch"));
            }
            sqlx::query("UPDATE join_requests SET decision_id=NULL, status=CASE WHEN expires_at<=now() THEN 'expired' ELSE 'pending' END, terminal_at=CASE WHEN expires_at<=now() THEN expires_at ELSE NULL END WHERE request_id=$1")
                .bind(target.2).execute(&mut *tx).await?;
        }
        "accept" => {
            if previous != Some(body.decision_id)
                || row.get::<Option<String>, _>("name").is_some_and(|name| {
                    Some(name.as_str()) != body.receipt.as_ref().map(|r| r.name.as_str())
                })
            {
                return Err(ApiError(StatusCode::CONFLICT, "decision_mismatch"));
            }
            sqlx::query("UPDATE join_requests SET status='accepted',terminal_at=now(),receipt=$2,message=NULL WHERE request_id=$1")
                .bind(target.2).bind(receipt).execute(&mut *tx).await?;
        }
        "reject" | "reject-block" => {
            if previous.is_some() {
                return Err(ApiError(StatusCode::CONFLICT, "decision_in_progress"));
            }
            sqlx::query("UPDATE join_requests SET status='rejected',terminal_at=now(),decision_id=$2,response=$3,blocked=$4 WHERE request_id=$1")
                .bind(target.2).bind(body.decision_id).bind(body.response).bind(body.action == "reject-block").execute(&mut *tx).await?;
        }
        _ => unreachable!(),
    }
    if body.action == "reject-block" {
        crate::moderation::block(
            &mut tx,
            (target.0, target.1),
            &row.get::<Vec<u8>, _>("installation_digest"),
            row.get::<Option<String>, _>("name")
                .as_deref()
                .unwrap_or("Rejected applicant"),
        )
        .await?;
    }
    let result = locked_request(&mut tx, target).await?;
    tx.commit().await?;
    Ok(Json(view(&result, true)))
}

pub async fn maintain(db: PgPool) {
    let mut timer = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        timer.tick().await;
        if cleanup(&db).await.is_err() {
            tracing::warn!("request_cleanup_failed");
        }
    }
}
