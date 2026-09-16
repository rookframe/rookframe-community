use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const ADMIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const APPLICANT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const OTHER: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    token: &str,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(format!("/api/v1{path}"))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:1234".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn published(app: &Router) -> String {
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    assert_eq!(request(app, "PUT", &path, json!({"operation_id":Uuid::new_v4(), "expected_revision":0,
        "listing":{"name":"Table", "description":"Weekly", "game_system":"System", "language":"English", "player_limit":5}}), ADMIN).await.0, StatusCode::OK);
    path
}

#[sqlx::test(migrations = "./migrations")]
async fn private_submission_is_retryable_and_one_active_request_per_installation(db: PgPool) {
    let app = rookframe_community::router(db);
    let world = published(&app).await;
    let id = Uuid::new_v4();
    let path = format!("{world}/requests/{id}");
    let body = json!({"name":"Mira", "message":"I would like to join."});
    let first = request(&app, "PUT", &path, body.clone(), APPLICANT).await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(first.1["status"], "pending");
    assert_eq!(
        request(&app, "PUT", &path, body.clone(), APPLICANT).await,
        first
    );
    assert_eq!(
        request(&app, "GET", &path, Value::Null, OTHER).await.0,
        StatusCode::NOT_FOUND
    );
    let different = format!("{world}/requests/{}", Uuid::new_v4());
    assert_eq!(
        request(&app, "PUT", &different, body.clone(), APPLICANT)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&app, "PUT", &different, body, OTHER).await.0,
        StatusCode::OK
    );
    let (_, public) = request(&app, "GET", &world, Value::Null, OTHER).await;
    assert!(!public.to_string().contains("Mira"));
    let review = request(
        &app,
        "GET",
        &format!("{world}/requests"),
        Value::Null,
        ADMIN,
    )
    .await;
    assert_eq!(review.1["requests"].as_array().unwrap().len(), 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn decision_recovery_fences_withdrawal_and_delivers_the_same_receipt(db: PgPool) {
    let app = rookframe_community::router(db);
    let world = published(&app).await;
    let id = Uuid::new_v4();
    let path = format!("{world}/requests/{id}");
    request(
        &app,
        "PUT",
        &path,
        json!({"name":"Mira","message":"Private message"}),
        APPLICANT,
    )
    .await;
    let decision = format!("{path}/decision");
    let operation = Uuid::new_v4();
    let prepare = json!({"decision_id":operation,"action":"prepare"});
    assert_eq!(
        request(&app, "PUT", &decision, prepare.clone(), OTHER)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "PUT", &decision, prepare.clone(), ADMIN)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &format!("{path}/withdraw"),
            Value::Null,
            APPLICANT
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&app, "PUT", &decision, prepare, ADMIN).await.0,
        StatusCode::OK
    );
    let accepted = json!({"decision_id":operation,"action":"accept", "receipt":{"seat_id":id,"name":"Mira","credential":OTHER}});
    let first = request(&app, "PUT", &decision, accepted.clone(), ADMIN).await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(
        request(&app, "PUT", &decision, accepted, ADMIN).await,
        first
    );
    let (_, status) = request(&app, "GET", &path, Value::Null, APPLICANT).await;
    assert_eq!(status["status"], "accepted");
    assert_eq!(status["receipt"]["credential"], OTHER);
    assert_eq!(
        request(&app, "GET", &path, Value::Null, OTHER).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, retried) = request(
        &app,
        "PUT",
        &path,
        json!({"name":"Mira","message":"Private message"}),
        APPLICANT,
    )
    .await;
    assert_eq!(retried["status"], "accepted");
}

#[sqlx::test(migrations = "./migrations")]
async fn rejection_withdrawal_and_expiry_allow_reapplication_and_terminal_state_is_deleted(
    db: PgPool,
) {
    let app = rookframe_community::router(db.clone());
    let world = published(&app).await;
    for ending in ["rejected", "withdrawn", "expired"] {
        let id = Uuid::new_v4();
        let path = format!("{world}/requests/{id}");
        assert_eq!(
            request(
                &app,
                "PUT",
                &path,
                json!({"name":"Mira","message":"Private"}),
                APPLICANT
            )
            .await
            .0,
            StatusCode::OK
        );
        match ending {
            "rejected" => {
                let decision = json!({"decision_id":Uuid::new_v4(),"action":"reject","response":"A different schedule would fit better."});
                assert_eq!(
                    request(&app, "PUT", &format!("{path}/decision"), decision, ADMIN)
                        .await
                        .0,
                    StatusCode::OK
                );
            }
            "withdrawn" => {
                assert_eq!(
                    request(
                        &app,
                        "POST",
                        &format!("{path}/withdraw"),
                        Value::Null,
                        APPLICANT
                    )
                    .await
                    .0,
                    StatusCode::OK
                );
            }
            _ => {
                sqlx::query("UPDATE join_requests SET expires_at=now()-interval '1 second' WHERE request_id=$1").bind(id).execute(&db).await.unwrap();
            }
        }
        let (_, status) = request(&app, "GET", &path, Value::Null, APPLICANT).await;
        assert_eq!(status["status"], ending);
        if ending == "rejected" {
            assert_eq!(status["response"], "A different schedule would fit better.");
        }
        if ending == "expired" {
            assert!(status["message"].is_null());
        }
        sqlx::query(
            "UPDATE join_requests SET terminal_at=now()-interval '31 days' WHERE request_id=$1",
        )
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(
            request(&app, "GET", &path, Value::Null, APPLICANT).await.0,
            StatusCode::NOT_FOUND
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn abort_preserves_pending_without_a_seat_and_a_different_decision_can_retry(db: PgPool) {
    let app = rookframe_community::router(db);
    let world = published(&app).await;
    let path = format!("{world}/requests/{}", Uuid::new_v4());
    request(
        &app,
        "PUT",
        &path,
        json!({"name":"Mira","message":"Private"}),
        APPLICANT,
    )
    .await;
    let decision = format!("{path}/decision");
    let id = Uuid::new_v4();
    assert_eq!(
        request(
            &app,
            "PUT",
            &decision,
            json!({"decision_id":id,"action":"prepare"}),
            ADMIN
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "PUT",
            &decision,
            json!({"decision_id":id,"action":"abort"}),
            ADMIN
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, status) = request(&app, "GET", &path, Value::Null, APPLICANT).await;
    assert_eq!(status["status"], "pending");
    assert!(status["decision_id"].is_null());
    let (_, listing) = request(&app, "GET", &world, Value::Null, APPLICANT).await;
    assert_eq!(listing["reserved_seats"], 0);
    assert_eq!(
        request(
            &app,
            "PUT",
            &decision,
            json!({"decision_id":Uuid::new_v4(),"action":"prepare"}),
            ADMIN
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_submissions_cannot_exceed_the_hundred_pending_limit(db: PgPool) {
    let app = rookframe_community::router(db.clone());
    let world = published(&app).await;
    let parts: Vec<_> = world.split('/').collect();
    let world_id = Uuid::parse_str(parts[2]).unwrap();
    let address = Uuid::parse_str(parts[3]).unwrap();
    for _ in 0..99 {
        sqlx::query("INSERT INTO join_requests(request_id,world_id,world_address,installation_digest,submission_digest,name,message) VALUES($1,$2,$3,$4,$4,'Mira',repeat('界',4000))")
            .bind(Uuid::new_v4()).bind(world_id).bind(address).bind(Uuid::new_v4().as_bytes().to_vec()).execute(&db).await.unwrap();
    }
    let first = format!("{world}/requests/{}", Uuid::new_v4());
    let second = format!("{world}/requests/{}", Uuid::new_v4());
    let (a, b) = tokio::join!(
        request(
            &app,
            "PUT",
            &first,
            json!({"name":"Mira","message":"Private"}),
            APPLICANT
        ),
        request(
            &app,
            "PUT",
            &second,
            json!({"name":"Mira","message":"Private"}),
            OTHER
        )
    );
    assert_eq!(
        [a.0, b.0].iter().filter(|s| **s == StatusCode::OK).count(),
        1
    );
    assert_eq!(
        if a.0 == StatusCode::OK { b.1 } else { a.1 }["error"],
        "request_queue_full"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM join_requests WHERE status='pending'")
            .fetch_one(&db)
            .await
            .unwrap(),
        100
    );
    let (_, review) = request(
        &app,
        "GET",
        &format!("{world}/requests"),
        Value::Null,
        ADMIN,
    )
    .await;
    assert!(review.to_string().len() > 1024 * 1024);
    assert_eq!(review["requests"].as_array().unwrap().len(), 100);
}

#[sqlx::test(migrations = "./migrations")]
async fn concurrent_withdrawal_and_acceptance_preparation_have_one_winner(db: PgPool) {
    let app = rookframe_community::router(db);
    let world = published(&app).await;
    let path = format!("{world}/requests/{}", Uuid::new_v4());
    request(
        &app,
        "PUT",
        &path,
        json!({"name":"Mira","message":"Private"}),
        APPLICANT,
    )
    .await;
    let operation = Uuid::new_v4();
    let withdraw_path = format!("{path}/withdraw");
    let decision_path = format!("{path}/decision");
    let (withdrawn, prepared) = tokio::join!(
        request(&app, "POST", &withdraw_path, Value::Null, APPLICANT),
        request(
            &app,
            "PUT",
            &decision_path,
            json!({"decision_id":operation,"action":"prepare"}),
            ADMIN
        )
    );
    assert_eq!(
        [withdrawn.0, prepared.0]
            .iter()
            .filter(|s| **s == StatusCode::OK)
            .count(),
        1
    );
    let (_, result) = request(&app, "GET", &path, Value::Null, APPLICANT).await;
    assert!(result["status"] == "withdrawn" || result["decision_id"] == operation.to_string());
}

#[sqlx::test(migrations = "./migrations")]
async fn installation_and_network_rate_limits_use_atomic_database_counters(db: PgPool) {
    use sha2::{Digest, Sha256};
    let app = rookframe_community::router(db.clone());
    let world = published(&app).await;
    let path = format!("{world}/requests/{}", Uuid::new_v4());
    for (scope, identity, maximum) in [
        ("submit_installation", APPLICANT, 10),
        ("submit_network", "127.0.0.1", 100),
    ] {
        sqlx::query("DELETE FROM request_rate_limits")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO request_rate_limits VALUES($1,$2,date_trunc('hour',now()),$3)")
            .bind(scope)
            .bind(Sha256::digest(identity.as_bytes()).to_vec())
            .bind(maximum)
            .execute(&db)
            .await
            .unwrap();
        let result = request(
            &app,
            "PUT",
            &path,
            json!({"name":"Mira","message":"Private"}),
            APPLICANT,
        )
        .await;
        assert_eq!(result.0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(result.1["error"], "request_rate_limited");
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM join_requests")
                .fetch_one(&db)
                .await
                .unwrap(),
            0
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn interrupted_decision_scrubs_recruitment_text_at_thirty_days_and_recovers_receipt(
    db: PgPool,
) {
    let app = rookframe_community::router(db.clone());
    let world = published(&app).await;
    let id = Uuid::new_v4();
    let path = format!("{world}/requests/{id}");
    request(
        &app,
        "PUT",
        &path,
        json!({"name":"Mira","message":"Private"}),
        APPLICANT,
    )
    .await;
    let operation = Uuid::new_v4();
    request(
        &app,
        "PUT",
        &format!("{path}/decision"),
        json!({"decision_id":operation,"action":"prepare"}),
        ADMIN,
    )
    .await;
    sqlx::query(
        "UPDATE join_requests SET expires_at=now()-interval '1 second' WHERE request_id=$1",
    )
    .bind(id)
    .execute(&db)
    .await
    .unwrap();
    let (_, result) = request(&app, "GET", &path, Value::Null, APPLICANT).await;
    assert!(result["name"].is_null() && result["message"].is_null());
    let receipt = json!({"seat_id":id,"name":"Mira","credential":OTHER});
    assert_eq!(
        request(
            &app,
            "PUT",
            &format!("{path}/decision"),
            json!({"decision_id":operation,"action":"accept","receipt":receipt}),
            ADMIN
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &path, Value::Null, APPLICANT).await.1["receipt"],
        receipt
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn rejection_block_is_private_and_explicitly_reversible(db: PgPool) {
    let app = rookframe_community::router(db);
    let world = published(&app).await;
    let id = Uuid::new_v4();
    let path = format!("{world}/requests/{id}");
    let body = json!({"name":"Mira","message":"Private application"});
    assert_eq!(
        request(&app, "PUT", &path, body.clone(), APPLICANT).await.0,
        StatusCode::OK
    );
    let decision =
        json!({"decision_id":Uuid::new_v4(),"action":"reject-block","response":"No thanks"});
    assert_eq!(
        request(
            &app,
            "PUT",
            &format!("{path}/decision"),
            decision.clone(),
            ADMIN
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "PUT", &format!("{path}/decision"), decision, ADMIN)
            .await
            .0,
        StatusCode::OK
    );
    let next = format!("{world}/requests/{}", Uuid::new_v4());
    assert_eq!(
        request(&app, "PUT", &next, body.clone(), APPLICANT).await.1["error"],
        "installation_blocked"
    );
    assert_eq!(
        request(&app, "GET", &format!("{world}/blocks"), Value::Null, OTHER)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let blocks = request(&app, "GET", &format!("{world}/blocks"), Value::Null, ADMIN)
        .await
        .1;
    let hash = blocks["blocks"][0]["installation_hash"].as_str().unwrap();
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("{world}/blocks/{hash}"),
            Value::Null,
            ADMIN
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "PUT", &next, body, APPLICANT).await.0,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn removal_scrubs_receipts_allows_reapplication_and_retry_cannot_restore_a_block(db: PgPool) {
    use sha2::{Digest, Sha256};
    let app = rookframe_community::router(db);
    let world = published(&app).await;
    let id = Uuid::new_v4();
    let path = format!("{world}/requests/{id}");
    let body = json!({"name":"Mira","message":"Private"});
    assert_eq!(
        request(&app, "PUT", &path, body.clone(), APPLICANT).await.0,
        StatusCode::OK
    );
    let operation = Uuid::new_v4();
    let decision = format!("{path}/decision");
    request(
        &app,
        "PUT",
        &decision,
        json!({"decision_id":operation,"action":"prepare"}),
        ADMIN,
    )
    .await;
    request(&app,"PUT",&decision,json!({"decision_id":operation,"action":"accept","receipt":{"seat_id":id,"name":"Mira","credential":OTHER}}),ADMIN).await;
    let hash = hex::encode(Sha256::digest(APPLICANT.as_bytes()));
    let removal = json!({"operation_id":Uuid::new_v4(),"installation_hashes":[hash],"name":"Mira","block":true});
    let endpoint = format!("{world}/players/{id}/removal");
    assert_eq!(
        request(&app, "PUT", &endpoint, removal.clone(), OTHER)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "PUT", &endpoint, removal.clone(), ADMIN)
            .await
            .0,
        StatusCode::OK
    );
    let result = request(&app, "GET", &path, Value::Null, APPLICANT).await.1;
    assert_eq!(result["removed"], true);
    assert!(result["receipt"].is_null());
    assert!(!result.to_string().contains(OTHER));
    assert_eq!(
        request(
            &app,
            "DELETE",
            &format!("{world}/blocks/{hash}"),
            Value::Null,
            ADMIN
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "PUT", &endpoint, removal, ADMIN).await.0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &app,
            "PUT",
            &format!("{world}/requests/{}", Uuid::new_v4()),
            body,
            APPLICANT
        )
        .await
        .0,
        StatusCode::OK
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn reports_are_bounded_private_and_only_an_operator_can_hide_service_presence(db: PgPool) {
    use sha2::{Digest, Sha256};
    let app = rookframe_community::router_with_moderation(
        db.clone(),
        rookframe_community::TurnProvider::disabled(),
        Some(Sha256::digest(OTHER.as_bytes()).to_vec()),
    );
    let world = published(&app).await;
    let id = Uuid::new_v4();
    let report = format!("{world}/reports/{id}");
    assert_eq!(
        request(
            &app,
            "PUT",
            &report,
            json!({"reason":"Abusive listing"}),
            APPLICANT
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", "/operator/reports", Value::Null, ADMIN)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let reports = request(&app, "GET", "/operator/reports", Value::Null, OTHER)
        .await
        .1;
    assert_eq!(reports["reports"].as_array().unwrap().len(), 1);
    assert!(!reports.to_string().contains("reporter_digest"));
    let moderation = format!("/operator{world}");
    assert_eq!(
        request(&app, "PUT", &moderation, json!({"hidden":true}), ADMIN)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&app, "PUT", &moderation, json!({"hidden":true}), OTHER)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &world, Value::Null, APPLICANT).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&app, "PUT", &moderation, json!({"hidden":false}), OTHER)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "GET", &world, Value::Null, APPLICANT).await.0,
        StatusCode::OK
    );
    let long = json!({"reason":"x".repeat(1001)});
    assert_eq!(
        request(&app, "PUT", &report, long, APPLICANT).await.0,
        StatusCode::BAD_REQUEST
    );
    sqlx::query("UPDATE abuse_reports SET created_at=now()-interval '31 days'")
        .execute(&db)
        .await
        .unwrap();
    request(
        &app,
        "PUT",
        &format!("{world}/requests/{}", Uuid::new_v4()),
        json!({"name":"Mira","message":"Private"}),
        APPLICANT,
    )
    .await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM abuse_reports")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}
