use crate::error::ApiError;
use axum::{
    Extension, Json,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capacity: Option<Capacity>,
}
#[derive(Deserialize)]
pub struct Search {
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: i64,
    limit: Option<i64>,
    system: Option<String>,
    language: Option<String>,
    online: Option<bool>,
    full: Option<bool>,
}

fn valid_id(id: Uuid) -> bool {
    id.get_version_num() == 4 && id.get_variant() == uuid::Variant::RFC4122
}

pub async fn publish(
    State(db): State<PgPool>,
    Extension(context): Extension<crate::DirectoryContext>,
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
    let capacity = change.capacity.clone().unwrap_or_default();
    capacity.validate()?;
    if change
        .listing
        .as_ref()
        .is_some_and(|l| capacity.reserved > l.player_limit || capacity.claimed >= l.player_limit)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_player_limit"));
    }
    let digest = Sha256::digest(token.as_bytes()).to_vec();
    let payload = serde_json::to_value(&change).expect("serializable mutation");
    let mut tx = db.begin().await?;
    // Competing first publications serialize on the primary key. The successful
    // first commit establishes ownership; removal never gives that ownership away.
    sqlx::query("INSERT INTO world_addresses (world_id,world_address,administrator_digest) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(world).bind(address).bind(&digest).execute(&mut *tx).await?;
    let current = sqlx::query("SELECT administrator_digest,revision,admission_revision,reserved_seats,claimed_seats FROM world_addresses WHERE world_id=$1 AND world_address=$2 FOR UPDATE")
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
    require_capacity_revision(&current, &capacity)?;
    let listing = change
        .listing
        .as_ref()
        .map(|l| serde_json::to_value(l).expect("serializable listing"));
    let result = json!({"operation_id":change.operation_id,"revision":revision+1,"visibility":if listing.is_some(){"public"}else{"private"}});
    sqlx::query("UPDATE world_addresses SET listing=$3,revision=revision+1,checked_in_at=$7,admission_revision=$4,reserved_seats=$5,claimed_seats=$6 WHERE world_id=$1 AND world_address=$2")
        .bind(world).bind(address).bind(listing).bind(capacity.revision).bind(capacity.reserved).bind(capacity.claimed).bind((context.clock)()).execute(&mut *tx).await?;
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
    Extension(context): Extension<crate::DirectoryContext>,
    Path((world, address)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT world_id,world_address,revision,listing,reserved_seats,claimed_seats,checked_in_at,concat(world_id,'/',world_address) = ANY($4) AS online FROM world_addresses WHERE world_id=$1 AND world_address=$2 AND NOT moderated AND listing IS NOT NULL AND checked_in_at > $3-interval '30 days'")
        .bind(world).bind(address).bind((context.clock)()).bind(context.availability.online()).fetch_optional(&db).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"listing_not_found"))?;
    Ok(Json(entry(row)))
}
pub async fn browse(
    State(db): State<PgPool>,
    Extension(context): Extension<crate::DirectoryContext>,
    Query(search): Query<Search>,
) -> Result<Json<Value>, ApiError> {
    let limit = search.limit.unwrap_or(30);
    if !(1..=50).contains(&limit)
        || !(0..=100000).contains(&search.offset)
        || search.q.encode_utf16().count() > 200
        || search
            .system
            .as_ref()
            .is_some_and(|s| s.encode_utf16().count() > 200)
        || search
            .language
            .as_ref()
            .is_some_and(|s| s.encode_utf16().count() > 100)
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
    let rows = sqlx::query("SELECT world_id,world_address,revision,listing,reserved_seats,claimed_seats,checked_in_at,concat(world_id,'/',world_address) = ANY($4) AS online FROM world_addresses WHERE NOT moderated AND listing IS NOT NULL AND checked_in_at > $5-interval '30 days' AND concat_ws(' ',listing->>'name',listing->>'description',listing->>'game_system',listing->>'language',listing->>'schedule') ILIKE $1 AND ($6::text IS NULL OR lower(listing->>'game_system')=lower($6)) AND ($7::text IS NULL OR lower(listing->>'language')=lower($7)) AND ($8::boolean IS NULL OR (concat(world_id,'/',world_address) = ANY($4))=$8) AND ($9::boolean IS NULL OR (reserved_seats >= (listing->>'player_limit')::int)=$9) ORDER BY CASE WHEN reserved_seats >= (listing->>'player_limit')::int THEN 2 WHEN concat(world_id,'/',world_address) = ANY($4) THEN 0 ELSE 1 END,checked_in_at DESC,world_id,world_address LIMIT $2 OFFSET $3")
        .bind(pattern).bind(limit+1).bind(search.offset).bind(context.availability.online())
        .bind((context.clock)()).bind(search.system).bind(search.language).bind(search.online).bind(search.full)
        .fetch_all(&db).await?;
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
    let listing: Value = row.get("listing");
    json!({"online": row.get::<bool,_>("online"), "checked_in_at": row.get::<chrono::DateTime<chrono::Utc>,_>("checked_in_at"), "full": row.get::<i32,_>("reserved_seats") >= listing["player_limit"].as_i64().unwrap_or(0) as i32, "reserved_seats":row.get::<i32,_>("reserved_seats"), "claimed_seats":row.get::<i32,_>("claimed_seats"), "world_id":row.get::<Uuid,_>("world_id"),"world_address":row.get::<Uuid,_>("world_address"),"revision":row.get::<i64,_>("revision"),"listing":row.get::<Value,_>("listing")})
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capacity {
    revision: i64,
    reserved: i32,
    claimed: i32,
}
impl Capacity {
    fn validate(&self) -> Result<(), ApiError> {
        if self.revision < 0
            || self.revision == i64::MAX
            || !(0..=1000).contains(&self.reserved)
            || !(0..=self.reserved).contains(&self.claimed)
        {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_capacity"));
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapacityUpdate {
    expected_directory_revision: i64,
    capacity: Capacity,
    remove_listing: bool,
}

fn require_capacity_revision(
    row: &sqlx::postgres::PgRow,
    value: &Capacity,
) -> Result<(), ApiError> {
    let revision: i64 = row.get("admission_revision");
    if value.revision < revision
        || value.revision == revision
            && (value.reserved != row.get::<i32, _>("reserved_seats")
                || value.claimed != row.get::<i32, _>("claimed_seats"))
    {
        return Err(ApiError(StatusCode::CONFLICT, "capacity_conflict"));
    }
    Ok(())
}

pub async fn capacity(
    State(db): State<PgPool>,
    Path((world, address)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(change): Json<CapacityUpdate>,
) -> Result<Json<Value>, ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "administrator_required"))?;
    change.capacity.validate()?;
    let mut tx = db.begin().await?;
    let row = sqlx::query("SELECT administrator_digest,revision,listing,admission_revision,reserved_seats,claimed_seats FROM world_addresses WHERE world_id=$1 AND world_address=$2 FOR UPDATE")
        .bind(world).bind(address).fetch_optional(&mut *tx).await?
        .ok_or(ApiError(StatusCode::NOT_FOUND,"listing_not_found"))?;
    if row.get::<Vec<u8>, _>("administrator_digest") != Sha256::digest(token.as_bytes()).to_vec() {
        return Err(ApiError(StatusCode::FORBIDDEN, "administrator_mismatch"));
    }
    if row.get::<i64, _>("revision") != change.expected_directory_revision {
        return Err(ApiError(StatusCode::CONFLICT, "revision_conflict"));
    }
    require_capacity_revision(&row, &change.capacity)?;
    let listing: Option<Value> = row.get("listing");
    if !change.remove_listing
        && listing.as_ref().is_some_and(|l| {
            change.capacity.reserved as i64 > l["player_limit"].as_i64().unwrap_or(0)
        })
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_capacity"));
    }
    let fully_claimed = listing.as_ref().is_some_and(|l| {
        change.capacity.claimed as i64 >= l["player_limit"].as_i64().unwrap_or(i64::MAX)
    });
    sqlx::query("UPDATE world_addresses SET admission_revision=$3,reserved_seats=$4,claimed_seats=$5,listing=CASE WHEN $6 THEN NULL ELSE listing END WHERE world_id=$1 AND world_address=$2")
        .bind(world).bind(address).bind(change.capacity.revision).bind(change.capacity.reserved)
        .bind(change.capacity.claimed).bind(change.remove_listing || fully_claimed).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({"revision":change.capacity.revision})))
}

// A new Community Server can confirm its own revision without contacting the old
// service. Removed/expired listings still retain their ownership reservation.
pub async fn administration(
    State(db): State<PgPool>,
    Path((world, address)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let digest = crate::requests::identity(&headers)?;
    let row = sqlx::query("SELECT administrator_digest,revision FROM world_addresses WHERE world_id=$1 AND world_address=$2")
        .bind(world).bind(address).fetch_optional(&db).await?;
    if let Some(row) = row {
        if row.get::<Vec<u8>, _>("administrator_digest") != digest {
            return Err(ApiError(StatusCode::FORBIDDEN, "administrator_mismatch"));
        }
        Ok(Json(json!({"revision":row.get::<i64,_>("revision")})))
    } else {
        Ok(Json(json!({"revision":0})))
    }
}
