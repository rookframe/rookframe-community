//! Temporary, bounded rendezvous only. This module never admits a Participant.
use crate::error::ApiError;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;
use uuid::Uuid;

type WorldKey = (Uuid, Uuid);
const LEASE_TTL: Duration = Duration::from_secs(30);
const ATTEMPT_TTL: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct SetupService {
    db: PgPool,
    book: Arc<Mutex<Book>>,
}
struct Book {
    leases: HashMap<WorldKey, Lease>,
    rate_start: Instant,
    requests: usize,
}
struct Lease {
    locator: Uuid,
    admin: Vec<u8>,
    expires: Instant,
    attempts: HashMap<Uuid, Attempt>,
    opened: usize,
    rate_start: Instant,
}
struct Attempt {
    digest: Vec<u8>,
    offer: Offer,
    answer: Option<Description>,
    peer_id: i32,
    expires: Instant,
    requests: usize,
}
#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    mid: String,
    index: i32,
    candidate: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Description {
    locator: Uuid,
    sdp: String,
    candidates: Vec<Candidate>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    peer_id: i32,
    locator: Uuid,
    sdp: String,
    candidates: Vec<Candidate>,
    proof: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Locator {
    locator: Uuid,
}

pub fn router(db: PgPool) -> Router {
    let book = Arc::new(Mutex::new(Book {
        leases: HashMap::new(),
        rate_start: Instant::now(),
        requests: 0,
    }));
    let weak = Arc::downgrade(&book);
    // The router owns the lifetime. Expiry also runs on every operation, so an
    // expired value can never be returned between cleanup ticks.
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(5));
        loop {
            timer.tick().await;
            let Some(book) = weak.upgrade() else { break };
            if let Ok(mut book) = book.lock() {
                book.expire(Instant::now());
            }
        }
    });
    Router::new()
        .route(
            "/api/v1/setup/worlds/{world}/{address}",
            get(resolve).put(publish).delete(revoke),
        )
        .route(
            "/api/v1/setup/worlds/{world}/{address}/connections",
            get(pending),
        )
        .route(
            "/api/v1/setup/worlds/{world}/{address}/connections/{attempt}",
            get(read).put(offer).delete(remove),
        )
        .route(
            "/api/v1/setup/worlds/{world}/{address}/connections/{attempt}/answer",
            axum::routing::put(answer),
        )
        .with_state(SetupService { db, book })
}

fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "world_unavailable")
}
fn busy() -> ApiError {
    ApiError(StatusCode::TOO_MANY_REQUESTS, "setup_limit")
}
fn invalid() -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, "invalid_setup")
}
fn valid_id(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}
fn token(headers: &HeaderMap) -> Result<Vec<u8>, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| valid_secret(v))
        .ok_or(ApiError(
            StatusCode::UNAUTHORIZED,
            "setup_credential_required",
        ))?;
    Ok(Sha256::digest(token.as_bytes()).to_vec())
}
fn valid_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn validate_description(
    locator: Uuid,
    sdp: &str,
    candidates: &[Candidate],
) -> Result<(), ApiError> {
    if !valid_id(locator)
        || sdp.is_empty()
        || sdp.len() > 16384
        || candidates.len() > 32
        || candidates.iter().any(|c| {
            c.mid.len() > 64
                || c.index < 0
                || c.index > 32
                || c.candidate.is_empty()
                || c.candidate.len() > 2048
                || c.mid.chars().any(char::is_control)
                || c.candidate.chars().any(char::is_control)
        })
    {
        return Err(invalid());
    }
    Ok(())
}
fn merge_candidates(retained: &mut Vec<Candidate>, incoming: &[Candidate]) -> Result<(), ApiError> {
    let mut next = retained.clone();
    for candidate in incoming {
        if !next.contains(candidate) {
            next.push(candidate.clone());
        }
    }
    if next.len() > 32 {
        return Err(busy());
    }
    *retained = next;
    Ok(())
}
impl Book {
    fn expire(&mut self, now: Instant) {
        self.leases.retain(|_, lease| {
            lease.attempts.retain(|_, attempt| attempt.expires > now);
            lease.expires > now
        });
    }
    fn enter(&mut self) -> Result<(), ApiError> {
        let now = Instant::now();
        self.expire(now);
        if now.duration_since(self.rate_start) >= Duration::from_secs(1) {
            self.requests = 0;
            self.rate_start = now;
        }
        self.requests += 1;
        if self.requests > 200 {
            return Err(busy());
        }
        Ok(())
    }
    fn lease(&mut self, key: WorldKey) -> Result<&mut Lease, ApiError> {
        self.leases.get_mut(&key).ok_or_else(missing)
    }
}
impl Lease {
    fn authorize(&self, digest: &[u8]) -> Result<(), ApiError> {
        if self.admin != digest {
            return Err(ApiError(StatusCode::FORBIDDEN, "administrator_mismatch"));
        }
        Ok(())
    }
    fn locate(&self, locator: Uuid) -> Result<(), ApiError> {
        if self.locator != locator {
            return Err(missing());
        }
        Ok(())
    }
}
impl Attempt {
    fn access(&mut self, digest: &[u8]) -> Result<(), ApiError> {
        if self.digest != digest {
            return Err(ApiError(StatusCode::FORBIDDEN, "setup_credential_mismatch"));
        }
        self.requests += 1;
        if self.requests > 150 {
            return Err(busy());
        }
        Ok(())
    }
    fn reply(&self, id: Uuid) -> Value {
        json!({"attempt_id":id,"peer_id":self.peer_id,"sdp":self.answer.as_ref().map(|a| &a.sdp),
            "candidates":self.answer.as_ref().map(|a| a.candidates.clone()).unwrap_or_default()})
    }
}
impl SetupService {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Book>, ApiError> {
        let mut book = self
            .book
            .lock()
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "setup_unavailable"))?;
        book.enter()?;
        Ok(book)
    }
}

async fn publish(
    State(service): State<SetupService>,
    Path(key): Path<WorldKey>,
    headers: HeaderMap,
    Json(input): Json<Locator>,
) -> Result<Json<Value>, ApiError> {
    let digest = token(&headers)?;
    if !valid_id(key.0) || !valid_id(key.1) || key.0 == key.1 || !valid_id(input.locator) {
        return Err(invalid());
    }
    {
        let _book = service.lock()?;
    }
    let mut tx = service.db.begin().await?;
    // The same durable ownership reservation is used by Directory and setup.
    // Private startup does not publish a Directory Listing or change its revision.
    sqlx::query("INSERT INTO world_addresses (world_id,world_address,administrator_digest) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(key.0).bind(key.1).bind(&digest).execute(&mut *tx).await?;
    let current = sqlx::query("SELECT administrator_digest FROM world_addresses WHERE world_id=$1 AND world_address=$2 FOR UPDATE")
        .bind(key.0).bind(key.1).fetch_one(&mut *tx).await?;
    if current.get::<Vec<u8>, _>("administrator_digest") != digest {
        return Err(ApiError(StatusCode::FORBIDDEN, "administrator_mismatch"));
    }
    tx.commit().await?;
    let mut book = service.lock()?;
    if !book.leases.contains_key(&key) && book.leases.len() >= 512 {
        return Err(busy());
    }
    let now = Instant::now();
    let lease = book.leases.entry(key).or_insert_with(|| Lease {
        locator: input.locator,
        admin: digest,
        expires: now + LEASE_TTL,
        attempts: HashMap::new(),
        opened: 0,
        rate_start: now,
    });
    // Another live epoch cannot be overwritten, including by a stale renewal.
    if lease.locator != input.locator {
        return Err(ApiError(StatusCode::CONFLICT, "world_already_running"));
    }
    lease.expires = now + LEASE_TTL;
    Ok(Json(
        json!({"world_id":key.0,"world_address":key.1,"locator":lease.locator}),
    ))
}
async fn resolve(
    State(service): State<SetupService>,
    Path(key): Path<WorldKey>,
) -> Result<Json<Value>, ApiError> {
    let mut book = service.lock()?;
    let lease = book.lease(key)?;
    Ok(Json(
        json!({"world_id":key.0,"world_address":key.1,"locator":lease.locator}),
    ))
}
async fn revoke(
    State(service): State<SetupService>,
    Path(key): Path<WorldKey>,
    headers: HeaderMap,
    Query(input): Query<Locator>,
) -> Result<Json<Value>, ApiError> {
    let digest = token(&headers)?;
    let mut book = service.lock()?;
    if let Some(lease) = book.leases.get(&key) {
        lease.authorize(&digest)?;
        lease.locate(input.locator)?;
    }
    book.leases.remove(&key);
    Ok(Json(json!({"removed":true})))
}
async fn pending(
    State(service): State<SetupService>,
    Path(key): Path<WorldKey>,
    headers: HeaderMap,
    Query(input): Query<Locator>,
) -> Result<Json<Value>, ApiError> {
    let digest = token(&headers)?;
    let mut book = service.lock()?;
    let lease = book.lease(key)?;
    lease.authorize(&digest)?;
    lease.locate(input.locator)?;
    let connections: Vec<Value> = lease
        .attempts
        .iter()
        .map(|(id, attempt)| {
            json!({"attempt_id":id,"peer_id":attempt.peer_id,
        "sdp":attempt.offer.sdp,"candidates":attempt.offer.candidates,"proof":attempt.offer.proof})
        })
        .collect();
    Ok(Json(json!({"connections":connections})))
}
async fn offer(
    State(service): State<SetupService>,
    Path((world, address, id)): Path<(Uuid, Uuid, Uuid)>,
    headers: HeaderMap,
    Json(input): Json<Offer>,
) -> Result<Json<Value>, ApiError> {
    let digest = token(&headers)?;
    validate_description(input.locator, &input.sdp, &input.candidates)?;
    if !valid_id(id) || input.peer_id <= 1 || !valid_secret(&input.proof) {
        return Err(invalid());
    }
    let mut book = service.lock()?;
    let lease = book.lease((world, address))?;
    lease.locate(input.locator)?;
    if let Some(attempt) = lease.attempts.get_mut(&id) {
        attempt.access(&digest)?;
        if attempt.peer_id != input.peer_id
            || attempt.offer.sdp != input.sdp
            || attempt.offer.proof != input.proof
        {
            return Err(ApiError(StatusCode::CONFLICT, "setup_conflict"));
        }
        merge_candidates(&mut attempt.offer.candidates, &input.candidates)?;
    } else {
        let now = Instant::now();
        if now.duration_since(lease.rate_start) >= Duration::from_secs(60) {
            lease.opened = 0;
            lease.rate_start = now;
        }
        if lease.attempts.len() >= 8 || lease.opened >= 16 {
            return Err(busy());
        }
        let peer_id = input.peer_id;
        if lease
            .attempts
            .values()
            .any(|attempt| attempt.peer_id == peer_id)
        {
            return Err(ApiError(StatusCode::CONFLICT, "peer_id_conflict"));
        }
        lease.opened += 1;
        lease.attempts.insert(
            id,
            Attempt {
                digest,
                offer: input,
                answer: None,
                peer_id,
                expires: now + ATTEMPT_TTL,
                requests: 1,
            },
        );
    }
    Ok(Json(lease.attempts[&id].reply(id)))
}
async fn read(
    State(service): State<SetupService>,
    Path((world, address, id)): Path<(Uuid, Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let digest = token(&headers)?;
    let mut book = service.lock()?;
    let attempt = book
        .lease((world, address))?
        .attempts
        .get_mut(&id)
        .ok_or_else(missing)?;
    attempt.access(&digest)?;
    Ok(Json(attempt.reply(id)))
}
async fn answer(
    State(service): State<SetupService>,
    Path((world, address, id)): Path<(Uuid, Uuid, Uuid)>,
    headers: HeaderMap,
    Json(input): Json<Description>,
) -> Result<Json<Value>, ApiError> {
    let digest = token(&headers)?;
    validate_description(input.locator, &input.sdp, &input.candidates)?;
    let mut book = service.lock()?;
    let lease = book.lease((world, address))?;
    lease.authorize(&digest)?;
    lease.locate(input.locator)?;
    let attempt = lease.attempts.get_mut(&id).ok_or_else(missing)?;
    if let Some(answer) = &mut attempt.answer {
        if answer.sdp != input.sdp {
            return Err(ApiError(StatusCode::CONFLICT, "setup_conflict"));
        }
        merge_candidates(&mut answer.candidates, &input.candidates)?;
    } else {
        attempt.answer = Some(input);
    }
    Ok(Json(attempt.reply(id)))
}
async fn remove(
    State(service): State<SetupService>,
    Path((world, address, id)): Path<(Uuid, Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let digest = token(&headers)?;
    let mut book = service.lock()?;
    let lease = book.lease((world, address))?;
    if lease.admin != digest
        && let Some(attempt) = lease.attempts.get_mut(&id)
    {
        attempt.access(&digest)?;
    }
    lease.attempts.remove(&id);
    Ok(Json(json!({"removed":true})))
}
