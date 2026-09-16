use crate::{
    error::ApiError,
    requests::{administrator, identity, lock_world, rate, secret, valid_id, valid_text},
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, put},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::net::SocketAddr;
use uuid::Uuid;

pub fn router(db: PgPool, operator: Option<Vec<u8>>) -> Router {
    Router::new()
        .route("/api/v1/worlds/{world}/{address}/blocks", get(blocks))
        .route(
            "/api/v1/worlds/{world}/{address}/blocks/{installation}",
            axum::routing::delete(unblock),
        )
        .route(
            "/api/v1/worlds/{world}/{address}/players/{seat}/removal",
            put(remove),
        )
        .route(
            "/api/v1/worlds/{world}/{address}/reports/{report}",
            put(report),
        )
        .route("/api/v1/operator/reports", get(reports))
        .route("/api/v1/operator/worlds/{world}/{address}", put(moderate))
        .with_state((db, operator))
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
type Service = (PgPool, Option<Vec<u8>>);

pub(crate) async fn block(
    tx: &mut Transaction<'_, Postgres>,
    target: (Uuid, Uuid),
    digest: &[u8],
    name: &str,
) -> Result<(), ApiError> {
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM installation_blocks WHERE world_id=$1 AND world_address=$2 AND installation_digest<>$3")
        .bind(target.0).bind(target.1).bind(digest).fetch_one(&mut **tx).await?;
    if count >= 1000 {
        return Err(ApiError(StatusCode::CONFLICT, "block_limit"));
    }
    sqlx::query("INSERT INTO installation_blocks(world_id,world_address,installation_digest,name) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
        .bind(target.0).bind(target.1).bind(digest).bind(name).execute(&mut **tx).await?;
    Ok(())
}
async fn blocks(
    State((db, _)): State<Service>,
    Path(target): Path<(Uuid, Uuid)>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    rate(&db, &digest, Some(network.0), &headers, false).await?;
    let mut tx = db.begin().await?;
    administrator(&lock_world(&mut tx, target).await?, &digest)?;
    let rows=sqlx::query("SELECT installation_digest,name,created_at FROM installation_blocks WHERE world_id=$1 AND world_address=$2 ORDER BY created_at DESC LIMIT 1000")
        .bind(target.0).bind(target.1).fetch_all(&mut *tx).await?;
    Ok(Json(
        json!({"blocks":rows.iter().map(|r|json!({"installation_hash":hex::encode(r.get::<Vec<u8>,_>("installation_digest")),"name":r.get::<String,_>("name"),"created_at":r.get::<chrono::DateTime<chrono::Utc>,_>("created_at")})).collect::<Vec<_>>()}),
    ))
}
async fn unblock(
    State((db, _)): State<Service>,
    Path((world, address, hash)): Path<(Uuid, Uuid, String)>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    rate(&db, &digest, Some(network.0), &headers, true).await?;
    if !secret(&hash) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_block"));
    }
    let mut tx = db.begin().await?;
    administrator(&lock_world(&mut tx, (world, address)).await?, &digest)?;
    sqlx::query("DELETE FROM installation_blocks WHERE world_id=$1 AND world_address=$2 AND installation_digest=$3")
        .bind(world).bind(address).bind(hex::decode(hash).unwrap()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Removal {
    operation_id: Uuid,
    installation_hashes: Vec<String>,
    name: String,
    block: bool,
}
async fn remove(
    State((db, _)): State<Service>,
    Path((world, address, seat)): Path<(Uuid, Uuid, Uuid)>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Removal>,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    rate(&db, &digest, Some(network.0), &headers, false).await?;
    if !valid_id(seat)
        || !valid_id(body.operation_id)
        || body.installation_hashes.len() > 33
        || body.installation_hashes.iter().any(|s| !secret(s))
        || !valid_text(&body.name, 100, false)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_removal"));
    }
    let payload = Sha256::digest(serde_json::to_vec(&(seat, &body)).unwrap()).to_vec();
    let mut tx = db.begin().await?;
    administrator(&lock_world(&mut tx, (world, address)).await?, &digest)?;
    if let Some(saved)=sqlx::query_scalar::<_,Vec<u8>>("SELECT payload_digest FROM player_removals WHERE world_id=$1 AND world_address=$2 AND operation_id=$3")
        .bind(world).bind(address).bind(body.operation_id).fetch_optional(&mut *tx).await? {
        if saved!=payload { return Err(ApiError(StatusCode::CONFLICT,"operation_conflict")); }
        return Ok(Json(json!({"ok":true})));
    }
    if body.block {
        for hash in &body.installation_hashes {
            block(
                &mut tx,
                (world, address),
                &hex::decode(hash).unwrap(),
                &body.name,
            )
            .await?;
        }
    }
    sqlx::query("UPDATE join_requests SET removed=true,receipt=NULL WHERE world_id=$1 AND world_address=$2 AND request_id=$3 AND status='accepted'")
        .bind(world).bind(address).bind(seat).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO player_removals(world_id,world_address,operation_id,payload_digest) VALUES($1,$2,$3,$4)")
        .bind(world).bind(address).bind(body.operation_id).bind(payload).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    request_id: Option<Uuid>,
    reason: String,
}
async fn report(
    State((db, _)): State<Service>,
    Path((world, address, id)): Path<(Uuid, Uuid, Uuid)>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Report>,
) -> Result<Json<Value>, ApiError> {
    let digest = identity(&headers)?;
    rate(&db, &digest, Some(network.0), &headers, true).await?;
    if !valid_id(id) || !valid_text(&body.reason, 1000, true) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_report"));
    }
    let mut tx = db.begin().await?;
    let listing = lock_world(&mut tx, (world, address)).await?;
    if let Some(request) = body.request_id {
        administrator(&listing, &digest)?;
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM join_requests WHERE request_id=$1 AND world_id=$2 AND world_address=$3)")
            .bind(request).bind(world).bind(address).fetch_one(&mut *tx).await?;
        if !exists {
            return Err(ApiError(StatusCode::NOT_FOUND, "request_not_found"));
        }
    } else if listing.get::<Option<Value>, _>("listing").is_none()
        || listing.get::<bool, _>("moderated")
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "listing_not_found"));
    }
    if let Some(saved) = sqlx::query("SELECT * FROM abuse_reports WHERE report_id=$1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
    {
        if saved.get::<Uuid, _>("world_id") != world
            || saved.get::<Uuid, _>("world_address") != address
            || saved.get::<Vec<u8>, _>("reporter_digest") != digest
            || saved.get::<String, _>("reason") != body.reason
            || saved.get::<Option<Uuid>, _>("request_id") != body.request_id
        {
            return Err(ApiError(StatusCode::CONFLICT, "operation_conflict"));
        }
    } else {
        sqlx::query("INSERT INTO abuse_reports(report_id,world_id,world_address,request_id,reporter_digest,reason) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(id).bind(world).bind(address).bind(body.request_id).bind(digest).bind(body.reason).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
async fn require_operator(
    db: &PgPool,
    configured: Option<Vec<u8>>,
    headers: &HeaderMap,
    network: SocketAddr,
) -> Result<(), ApiError> {
    let digest = identity(headers)?;
    rate(db, &digest, Some(network), headers, false).await?;
    if configured.as_deref() != Some(digest.as_slice()) {
        return Err(ApiError(StatusCode::FORBIDDEN, "operator_required"));
    }
    Ok(())
}
async fn reports(
    State((db, operator)): State<Service>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_operator(&db, operator, &headers, network.0).await?;
    let rows=sqlx::query("SELECT report_id,world_id,world_address,request_id,reason,created_at FROM abuse_reports WHERE NOT resolved AND created_at>now()-interval '30 days' ORDER BY created_at LIMIT 100").fetch_all(&db).await?;
    Ok(Json(
        json!({"reports":rows.iter().map(|r|json!({"report_id":r.get::<Uuid,_>("report_id"),"world_id":r.get::<Uuid,_>("world_id"),"world_address":r.get::<Uuid,_>("world_address"),"request_id":r.get::<Option<Uuid>,_>("request_id"),"reason":r.get::<String,_>("reason"),"created_at":r.get::<chrono::DateTime<chrono::Utc>,_>("created_at")})).collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Moderation {
    hidden: bool,
}
async fn moderate(
    State((db, operator)): State<Service>,
    Path(target): Path<(Uuid, Uuid)>,
    network: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Moderation>,
) -> Result<Json<Value>, ApiError> {
    require_operator(&db, operator, &headers, network.0).await?;
    let mut tx = db.begin().await?;
    lock_world(&mut tx, target).await?;
    sqlx::query("UPDATE world_addresses SET moderated=$3 WHERE world_id=$1 AND world_address=$2")
        .bind(target.0)
        .bind(target.1)
        .bind(body.hidden)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE abuse_reports SET resolved=true WHERE world_id=$1 AND world_address=$2")
        .bind(target.0)
        .bind(target.1)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}
