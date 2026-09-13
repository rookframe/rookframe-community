use crate::error::ApiError;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Listing {
    name: String,
    description: String,
    game_system: String,
    language: String,
    player_limit: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cover_image: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mutation {
    operation_id: Uuid,
    expected_revision: i64,
    listing: Option<Listing>,
}
#[derive(Deserialize)]
pub struct Search {
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: i64,
    limit: Option<i64>,
}

fn valid_id(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

pub async fn publish(
    State(db): State<PgPool>,
    Path((world, address)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(change): Json<Mutation>,
) -> Result<Json<Value>, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "administrator_required"))?;
    if !valid_id(world)
        || !valid_id(address)
        || world == address
        || !valid_id(change.operation_id)
        || change.expected_revision < 0
        || change.expected_revision == i64::MAX
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_identity_or_revision",
        ));
    }
    if let Some(listing) = &change.listing {
        listing.validate()?;
    }
    let digest = Sha256::digest(token.as_bytes()).to_vec();
    let payload = serde_json::to_value(&change).expect("serializable mutation");
    let mut tx = db.begin().await?;
    // Competing first publications serialize on the primary key. The successful
    // first commit establishes ownership; removal never gives that ownership away.
    sqlx::query("INSERT INTO world_addresses (world_id,world_address,administrator_digest) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(world).bind(address).bind(&digest).execute(&mut *tx).await?;
    let current = sqlx::query("SELECT administrator_digest,revision FROM world_addresses WHERE world_id=$1 AND world_address=$2 FOR UPDATE")
        .bind(world).bind(address).fetch_one(&mut *tx).await?;
    if current.get::<Vec<u8>, _>("administrator_digest") != digest {
        return Err(ApiError(StatusCode::FORBIDDEN, "administrator_mismatch"));
    }
    if let Some(previous) = sqlx::query("SELECT request,result FROM directory_operations WHERE world_id=$1 AND world_address=$2 AND operation_id=$3")
        .bind(world).bind(address).bind(change.operation_id).fetch_optional(&mut *tx).await? {
        if previous.get::<Value,_>("request") != payload { return Err(ApiError(StatusCode::CONFLICT,"operation_conflict")); }
        return Ok(Json(previous.get("result")));
    }
    let revision: i64 = current.get("revision");
    if revision != change.expected_revision {
        return Err(ApiError(StatusCode::CONFLICT, "revision_conflict"));
    }
    let listing = change
        .listing
        .as_ref()
        .map(|l| serde_json::to_value(l).expect("serializable listing"));
    let result = json!({"operation_id":change.operation_id,"revision":revision+1,"visibility":if listing.is_some(){"public"}else{"private"}});
    sqlx::query("UPDATE world_addresses SET listing=$3,revision=revision+1,checked_in_at=now() WHERE world_id=$1 AND world_address=$2")
        .bind(world).bind(address).bind(listing).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO directory_operations (world_id,world_address,operation_id,request,result) VALUES ($1,$2,$3,$4,$5)")
        .bind(world).bind(address).bind(change.operation_id).bind(payload).bind(&result).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(result))
}

impl Listing {
    fn validate(&self) -> Result<(), ApiError> {
        for (text, maximum, allow_line_breaks) in [
            (&self.name, 100, false),
            (&self.description, 4000, true),
            (&self.game_system, 200, false),
            (&self.language, 100, false),
        ] {
            if text.is_empty()
                || text.trim() != text
                || text.encode_utf16().count() > maximum
                || text.chars().any(|character| {
                    character.is_control()
                        && (!allow_line_breaks || !matches!(character, '\r' | '\n'))
                })
            {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_listing"));
            }
        }
        if !(1..=1000).contains(&self.player_limit) {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_player_limit"));
        }
        for (text, maximum) in [(&self.schedule, 500), (&self.cover_image, 2048)] {
            if text.as_ref().is_some_and(|s| {
                s.trim() != s
                    || s.encode_utf16().count() > maximum
                    || s.chars().any(char::is_control)
            }) {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_listing"));
            }
        }
        if self
            .cover_image
            .as_ref()
            .is_some_and(|s| match url::Url::parse(s) {
                Ok(url) => {
                    url.scheme() != "https"
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.fragment().is_some()
                }
                Err(_) => true,
            })
        {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_cover_image"));
        }
        Ok(())
    }
}

pub async fn read(
    State(db): State<PgPool>,
    Path((world, address)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT world_id,world_address,revision,listing FROM world_addresses WHERE world_id=$1 AND world_address=$2 AND listing IS NOT NULL AND checked_in_at > now()-interval '30 days'")
        .bind(world).bind(address).fetch_optional(&db).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"listing_not_found"))?;
    Ok(Json(entry(row)))
}
pub async fn browse(
    State(db): State<PgPool>,
    Query(search): Query<Search>,
) -> Result<Json<Value>, ApiError> {
    let limit = search.limit.unwrap_or(30);
    if !(1..=50).contains(&limit)
        || !(0..=100000).contains(&search.offset)
        || search.q.encode_utf16().count() > 200
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_search"));
    }
    let pattern = format!(
        "%{}%",
        search
            .q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let rows = sqlx::query("SELECT world_id,world_address,revision,listing FROM world_addresses WHERE listing IS NOT NULL AND checked_in_at > now()-interval '30 days' AND concat_ws(' ',listing->>'name',listing->>'description',listing->>'game_system',listing->>'language') ILIKE $1 ORDER BY checked_in_at DESC,world_id,world_address LIMIT $2 OFFSET $3")
        .bind(pattern).bind(limit+1).bind(search.offset).fetch_all(&db).await?;
    let next = if rows.len() > limit as usize {
        Some(search.offset + limit)
    } else {
        None
    };
    Ok(Json(
        json!({"listings":rows.into_iter().take(limit as usize).map(entry).collect::<Vec<_>>(),"next_offset":next}),
    ))
}
fn entry(row: sqlx::postgres::PgRow) -> Value {
    json!({"world_id":row.get::<Uuid,_>("world_id"),"world_address":row.get::<Uuid,_>("world_address"),"revision":row.get::<i64,_>("revision"),"listing":row.get::<Value,_>("listing")})
}
