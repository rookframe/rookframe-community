use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const ADMIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CLIENT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[sqlx::test(migrations = "./migrations")]
async fn relay_credentials_are_attempt_bound_and_setup_deletion_does_not_revoke_play(db: PgPool) {
    let turn = rookframe_community::TurnProvider::coturn(
        vec![
            "stun:relay.example:3478".into(),
            "turn:relay.example:3478?transport=udp".into(),
        ],
        "backend-only-secret-with-at-least-32-bytes".into(),
        21600,
    )
    .unwrap();
    let app = rookframe_community::router_with_turn(db, turn);
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    let locator = Uuid::new_v4();
    request(&app, "PUT", &path, json!({"locator":locator}), Some(ADMIN)).await;
    let connection = format!("{path}/connections/{}", Uuid::new_v4());
    let begin = json!({"locator":locator,"peer_id":42,"proof":CLIENT});
    let (status, ice) = request(&app, "POST", &connection, begin.clone(), Some(CLIENT)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        ice["ice_servers"][1]["urls"][0],
        "turn:relay.example:3478?transport=udp"
    );
    assert!(!ice.to_string().contains("backend-only"));
    assert!(ice["ice_servers"][1]["credential"].as_str().unwrap().len() > 20);
    assert_eq!(
        request(&app, "POST", &connection, begin.clone(), Some(CLIENT))
            .await
            .1,
        ice
    );
    assert_eq!(
        request(&app, "POST", &connection, begin, Some(ADMIN))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "DELETE", &connection, Value::Null, Some(CLIENT))
            .await
            .0,
        StatusCode::OK
    );
    // Signaling is temporary; a separate authorized operation revokes relay use.
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("{connection}/relay"),
            Value::Null,
            Some(CLIENT)
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn revocation_refuses_reissuance_without_claiming_coturn_provider_revocation(db: PgPool) {
    let provider = rookframe_community::TurnProvider::coturn(
        vec!["turn:relay.example:3478?transport=udp".into()],
        "backend-only-secret-with-at-least-32-bytes".into(),
        21600,
    )
    .unwrap();
    let app = rookframe_community::router_with_turn(db, provider);
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    let locator = Uuid::new_v4();
    request(&app, "PUT", &path, json!({"locator":locator}), Some(ADMIN)).await;
    let connection = format!("{path}/connections/{}", Uuid::new_v4());
    let begin = json!({"locator":locator,"peer_id":42,"proof":CLIENT});
    assert_eq!(
        request(&app, "POST", &connection, begin.clone(), Some(CLIENT))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("{connection}/ice"),
            json!({"locator":locator}),
            Some(CLIENT)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("{connection}/ice"),
            json!({"locator":locator}),
            Some(ADMIN)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, revoked) = request(
        &app,
        "DELETE",
        &format!("{connection}/relay"),
        Value::Null,
        Some(ADMIN),
    )
    .await;
    assert_eq!(
        revoked,
        json!({"issuance_revoked":true,"provider_revoked":false})
    );
    let (status, error) = request(&app, "POST", &connection, begin, Some(CLIENT)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error"], "relay_credential_revoked");

    // Revocation covers the authority side even before it requests a grant.
    let next = format!("{path}/connections/{}", Uuid::new_v4());
    reserve(
        &app,
        &next,
        &json!({"locator":locator,"peer_id":43,"proof":CLIENT}),
    )
    .await;
    request(
        &app,
        "DELETE",
        &format!("{next}/relay"),
        Value::Null,
        Some(ADMIN),
    )
    .await;
    let (status, error) = request(
        &app,
        "POST",
        &format!("{next}/ice"),
        json!({"locator":locator}),
        Some(ADMIN),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error"], "relay_credential_revoked");
    // Deleting signaling cannot turn the same revoked identity into a fresh grant.
    request(&app, "DELETE", &next, Value::Null, Some(CLIENT)).await;
    let begin = json!({"locator":locator,"peer_id":43,"proof":CLIENT});
    assert_eq!(
        request(&app, "POST", &next, begin, Some(CLIENT)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("{next}/ice"),
            json!({"locator":locator}),
            Some(ADMIN)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

async fn reserve(app: &Router, path: &str, offer: &Value) {
    let begin =
        json!({"locator":offer["locator"],"peer_id":offer["peer_id"],"proof":offer["proof"]});
    assert_eq!(
        request(app, "POST", path, begin, Some(CLIENT)).await.0,
        StatusCode::OK
    );
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    value: Value,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/v1/setup{path}"))
        .header("content-type", "application/json");
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(req.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[sqlx::test(migrations = "./migrations")]
async fn exact_private_world_setup_is_authenticated_and_removed_after_connection(db: PgPool) {
    let app = rookframe_community::router_with_turn(
        db,
        rookframe_community::TurnProvider::coturn(
            vec!["turn:relay.example:3478?transport=udp".into()],
            "backend-only-secret-with-at-least-32-bytes".into(),
            21600,
        )
        .unwrap(),
    );
    let world = Uuid::new_v4();
    let address = Uuid::new_v4();
    let locator = Uuid::new_v4();
    let path = format!("/worlds/{world}/{address}");
    assert_eq!(
        request(&app, "GET", &path, Value::Null, None).await.0,
        StatusCode::NOT_FOUND
    );
    let lease = json!({"locator":locator});
    assert_eq!(
        request(&app, "PUT", &path, lease.clone(), Some(ADMIN))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "PUT", &path, lease, Some(CLIENT)).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, resolved) = request(&app, "GET", &path, Value::Null, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(resolved["world_id"], world.to_string());
    assert_eq!(resolved["world_address"], address.to_string());
    assert_eq!(resolved["locator"], locator.to_string());
    assert!(!resolved.to_string().contains(ADMIN));
    let attempt = Uuid::new_v4();
    let connection = format!("{path}/connections/{attempt}");
    let offer = json!({"locator":locator,"peer_id":42,"sdp":"v=0\r\no=offer", "candidates":[],"proof":CLIENT});
    reserve(&app, &connection, &offer).await;
    let (status, created) = request(&app, "PUT", &connection, offer, Some(CLIENT)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(created["peer_id"].as_i64().unwrap() > 1);
    assert_eq!(
        request(&app, "GET", &connection, Value::Null, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (_, pending) = request(
        &app,
        "GET",
        &format!("{path}/connections?locator={locator}"),
        Value::Null,
        Some(ADMIN),
    )
    .await;
    assert_eq!(pending["connections"][0]["attempt_id"], attempt.to_string());
    let answer = json!({"locator":locator,"sdp":"v=0\r\no=answer", "candidates":[]});
    assert_eq!(
        request(
            &app,
            "PUT",
            &format!("{connection}/answer"),
            answer,
            Some(ADMIN)
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, answered) = request(&app, "GET", &connection, Value::Null, Some(CLIENT)).await;
    assert_eq!(answered["sdp"], "v=0\r\no=answer");
    assert_eq!(
        request(&app, "DELETE", &connection, Value::Null, Some(CLIENT))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &connection, Value::Null, Some(CLIENT))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, pending) = request(
        &app,
        "GET",
        &format!("{path}/connections?locator={locator}"),
        Value::Null,
        Some(ADMIN),
    )
    .await;
    assert_eq!(pending["connections"], json!([]));
}

#[sqlx::test(migrations = "./migrations")]
async fn old_epochs_and_other_credentials_cannot_change_pending_setup(db: PgPool) {
    let app = rookframe_community::router_with_turn(
        db,
        rookframe_community::TurnProvider::coturn(
            vec!["turn:relay.example:3478?transport=udp".into()],
            "backend-only-secret-with-at-least-32-bytes".into(),
            21600,
        )
        .unwrap(),
    );
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    let locator = Uuid::new_v4();
    let lease = json!({"locator":locator});
    assert_eq!(
        request(&app, "PUT", &path, lease.clone(), Some(ADMIN))
            .await
            .0,
        StatusCode::OK
    );
    let stale = Uuid::new_v4();
    assert_eq!(
        request(&app, "PUT", &path, json!({"locator":stale}), Some(ADMIN))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("{path}?locator={stale}"),
            Value::Null,
            Some(ADMIN)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let connection = format!("{path}/connections/{}", Uuid::new_v4());
    let offer = json!({"locator":locator,"peer_id":42,"sdp":"v=0","candidates":[],"proof":CLIENT});
    reserve(&app, &connection, &offer).await;
    assert_eq!(
        request(&app, "PUT", &connection, offer.clone(), Some(CLIENT))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &connection, Value::Null, Some(ADMIN))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut changed = offer;
    changed["sdp"] = json!("changed");
    assert_eq!(
        request(&app, "PUT", &connection, changed, Some(CLIENT))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("{path}?locator={locator}"),
            Value::Null,
            Some(ADMIN)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &connection, Value::Null, Some(CLIENT))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn setup_bounds_reject_peer_one_oversized_payloads_and_excess_concurrency(db: PgPool) {
    let app = rookframe_community::router_with_turn(
        db,
        rookframe_community::TurnProvider::coturn(
            vec!["turn:relay.example:3478?transport=udp".into()],
            "backend-only-secret-with-at-least-32-bytes".into(),
            21600,
        )
        .unwrap(),
    );
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    let locator = Uuid::new_v4();
    request(&app, "PUT", &path, json!({"locator":locator}), Some(ADMIN)).await;
    let connection = format!("{path}/connections/{}", Uuid::new_v4());
    let mut offer =
        json!({"locator":locator,"peer_id":1,"sdp":"v=0","candidates":[],"proof":CLIENT});
    assert_eq!(
        request(&app, "PUT", &connection, offer.clone(), Some(CLIENT))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    offer["peer_id"] = json!(42);
    offer["sdp"] = json!("x".repeat(16385));
    assert_eq!(
        request(&app, "PUT", &connection, offer.clone(), Some(CLIENT))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    offer["sdp"] = json!("x".repeat(40000));
    assert_eq!(
        request(&app, "PUT", &connection, offer.clone(), Some(CLIENT))
            .await
            .0,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    offer["sdp"] = json!("v=0");
    for peer in 2..10 {
        offer["peer_id"] = json!(peer);
        let next = format!("{path}/connections/{}", Uuid::new_v4());
        reserve(&app, &next, &offer).await;
        assert_eq!(
            request(&app, "PUT", &next, offer.clone(), Some(CLIENT))
                .await
                .0,
            StatusCode::OK
        );
    }
    offer["peer_id"] = json!(99);
    assert_eq!(
        request(
            &app,
            "POST",
            &connection,
            json!({"locator":locator,"peer_id":99,"proof":CLIENT}),
            Some(CLIENT)
        )
        .await
        .0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn expired_worlds_are_unresolvable_without_a_reader_triggering_cleanup(db: PgPool) {
    let app = rookframe_community::router_with_turn(
        db,
        rookframe_community::TurnProvider::coturn(
            vec!["turn:relay.example:3478?transport=udp".into()],
            "backend-only-secret-with-at-least-32-bytes".into(),
            21600,
        )
        .unwrap(),
    );
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    let locator = Uuid::new_v4();
    assert_eq!(
        request(&app, "PUT", &path, json!({"locator":locator}), Some(ADMIN))
            .await
            .0,
        StatusCode::OK
    );
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    assert_eq!(
        request(&app, "GET", &path, Value::Null, None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn candidates_merge_idempotently_and_attempts_expire_while_the_world_stays_online(
    db: PgPool,
) {
    let app = rookframe_community::router_with_turn(
        db,
        rookframe_community::TurnProvider::coturn(
            vec!["turn:relay.example:3478?transport=udp".into()],
            "backend-only-secret-with-at-least-32-bytes".into(),
            21600,
        )
        .unwrap(),
    );
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    let locator = Uuid::new_v4();
    let lease = json!({"locator":locator});
    request(&app, "PUT", &path, lease.clone(), Some(ADMIN)).await;
    let connection = format!("{path}/connections/{}", Uuid::new_v4());
    let mut offer =
        json!({"locator":locator,"peer_id":42,"sdp":"v=0","candidates":[],"proof":CLIENT});
    reserve(&app, &connection, &offer).await;
    request(&app, "PUT", &connection, offer.clone(), Some(CLIENT)).await;
    offer["candidates"] =
        json!([{"mid":"0","index":0,"candidate":"candidate:1 1 UDP 1 127.0.0.1 1234 typ host"}]);
    for _ in 0..2 {
        assert_eq!(
            request(&app, "PUT", &connection, offer.clone(), Some(CLIENT))
                .await
                .0,
            StatusCode::OK
        );
    }
    let (_, pending) = request(
        &app,
        "GET",
        &format!("{path}/connections?locator={locator}"),
        Value::Null,
        Some(ADMIN),
    )
    .await;
    assert_eq!(
        pending["connections"][0]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Advance the real production clock without sleeping or inspecting storage.
    tokio::time::pause();
    for _ in 0..3 {
        tokio::time::advance(std::time::Duration::from_secs(21)).await;
        tokio::time::resume();
        assert_eq!(
            request(&app, "PUT", &path, lease.clone(), Some(ADMIN))
                .await
                .0,
            StatusCode::OK
        );
        tokio::time::pause();
    }
    assert_eq!(
        request(&app, "GET", &connection, Value::Null, Some(CLIENT))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, "GET", &path, Value::Null, None).await.0,
        StatusCode::OK
    );
}
