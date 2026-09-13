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

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    value: Value,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(format!("/api/v1{path}"))
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

fn listing() -> Value {
    json!({"name":"The same table", "description":"Weekly adventures", "game_system":"Basic Fantasy", "language":"English", "player_limit":5})
}

#[sqlx::test(migrations = "./migrations")]
async fn paragraph_descriptions_round_trip_through_publication_and_anonymous_read(db: PgPool) {
    let app = rookframe_community::router(db);
    for line_break in ["\n", "\r\n", "\r"] {
        let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
        let description = format!("First paragraph.{line_break}{line_break}Second paragraph.");
        let mut data = listing();
        data["description"] = json!(description);
        let change = json!({"operation_id":Uuid::new_v4(),"expected_revision":0,"listing":data});
        assert_eq!(
            request(&app, "PUT", &path, change, Some(ADMIN)).await.0,
            StatusCode::OK
        );
        let (_, read) = request(&app, "GET", &path, Value::Null, None).await;
        assert_eq!(read["listing"]["description"], description);
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn only_description_accepts_line_breaks_and_other_controls_are_rejected(db: PgPool) {
    let app = rookframe_community::router(db);
    for field in [
        "name",
        "description",
        "game_system",
        "language",
        "schedule",
        "cover_image",
    ] {
        for control in ['\r', '\n', '\t', '\0', '\u{001b}', '\u{0085}'] {
            if field == "description" && matches!(control, '\r' | '\n') {
                continue;
            }
            let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
            let mut data = listing();
            data[field] = json!(if field == "cover_image" {
                format!("https://example.com/first{control}second.png")
            } else {
                format!("First{control}second")
            });
            let change =
                json!({"operation_id":Uuid::new_v4(),"expected_revision":0,"listing":data});
            assert_eq!(
                request(&app, "PUT", &path, change, Some(ADMIN)).await.0,
                StatusCode::BAD_REQUEST,
                "field {field}, control {control:?}"
            );
        }
    }
    let (_, page) = request(&app, "GET", "/worlds", Value::Null, None).await;
    assert!(page["listings"].as_array().unwrap().is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn paragraph_descriptions_still_require_trimmed_and_bounded_text(db: PgPool) {
    let app = rookframe_community::router(db);
    for description in [
        "\nParagraph".to_string(),
        "Paragraph\r".to_string(),
        "a".repeat(4001),
    ] {
        let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
        let mut data = listing();
        data["description"] = json!(description);
        let change = json!({"operation_id":Uuid::new_v4(),"expected_revision":0,"listing":data});
        assert_eq!(
            request(&app, "PUT", &path, change, Some(ADMIN)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn administrator_publishes_and_an_independent_reader_discovers_the_exact_world(db: PgPool) {
    let app = rookframe_community::router(db);
    let world = Uuid::new_v4();
    let address = Uuid::new_v4();
    let path = format!("/worlds/{world}/{address}");
    let mutation =
        json!({"operation_id":Uuid::new_v4(), "expected_revision":0, "listing":listing()});
    let (status, published) = request(&app, "PUT", &path, mutation, Some(ADMIN)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(published["revision"], 1);
    let (status, read) = request(&app, "GET", &path, Value::Null, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["world_id"], world.to_string());
    assert_eq!(read["world_address"], address.to_string());
    assert_eq!(read["listing"], listing());
    let (_, directory) = request(&app, "GET", "/worlds?q=adventures", Value::Null, None).await;
    assert_eq!(directory["listings"].as_array().unwrap().len(), 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn retries_are_exact_and_old_publication_cannot_resurrect_a_removed_listing(db: PgPool) {
    let app = rookframe_community::router(db);
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    let publish = json!({"operation_id":Uuid::new_v4(),"expected_revision":0,"listing":listing()});
    let first = request(&app, "PUT", &path, publish.clone(), Some(ADMIN)).await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(
        request(&app, "PUT", &path, publish.clone(), Some(ADMIN)).await,
        first
    );
    let remove = json!({"operation_id":Uuid::new_v4(),"expected_revision":1,"listing":null});
    assert_eq!(
        request(&app, "PUT", &path, remove.clone(), Some(ADMIN))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&app, "PUT", &path, publish.clone(), Some(ADMIN)).await,
        first
    );
    assert_eq!(
        request(&app, "GET", &path, Value::Null, None).await.0,
        StatusCode::NOT_FOUND
    );
    let mut changed = publish;
    changed["listing"]["name"] = json!("Different");
    assert_eq!(
        request(&app, "PUT", &path, changed, Some(ADMIN)).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&app, "PUT", &path, remove, Some(&"b".repeat(64)))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn simultaneous_first_publications_have_one_owner_and_names_do_not_reserve_identity(
    db: PgPool,
) {
    let app = rookframe_community::router(db);
    let world = Uuid::new_v4();
    let path = format!("/worlds/{world}/{}", Uuid::new_v4());
    let change = json!({"operation_id":Uuid::new_v4(),"expected_revision":0,"listing":listing()});
    let other = "b".repeat(64);
    let (left, right) = tokio::join!(
        request(&app, "PUT", &path, change.clone(), Some(ADMIN)),
        request(&app, "PUT", &path, change.clone(), Some(&other))
    );
    let mut codes = vec![left.0.as_u16(), right.0.as_u16()];
    codes.sort();
    assert_eq!(codes, vec![200, 403]);
    let copy = format!("/worlds/{world}/{}", Uuid::new_v4());
    assert_eq!(
        request(&app, "PUT", &copy, change, Some(ADMIN)).await.0,
        StatusCode::OK
    );
    let (_, page) = request(&app, "GET", "/worlds?limit=1", Value::Null, None).await;
    assert_eq!(page["listings"].as_array().unwrap().len(), 1);
    assert_eq!(page["next_offset"], 1);
    let (_, next) = request(&app, "GET", "/worlds?limit=1&offset=1", Value::Null, None).await;
    assert_ne!(
        page["listings"][0]["world_address"],
        next["listings"][0]["world_address"]
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn invalid_publication_leaves_previous_listing_and_revision_unchanged(db: PgPool) {
    let app = rookframe_community::router(db);
    let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
    let change = json!({"operation_id":Uuid::new_v4(),"expected_revision":0,"listing":listing()});
    assert_eq!(
        request(&app, "PUT", &path, change, Some(ADMIN)).await.0,
        StatusCode::OK
    );
    let invalid = json!({"operation_id":Uuid::new_v4(),"expected_revision":1,"listing":{"name":"","description":"","game_system":"X","language":"English","player_limit":0}});
    assert_eq!(
        request(&app, "PUT", &path, invalid, Some(ADMIN)).await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, current) = request(&app, "GET", &path, Value::Null, None).await;
    assert_eq!(current["revision"], 1);
    assert_eq!(current["listing"], listing());
    let stale = json!({"operation_id":Uuid::new_v4(),"expected_revision":0,"listing":null});
    assert_eq!(
        request(&app, "PUT", &path, stale, Some(ADMIN)).await.0,
        StatusCode::CONFLICT
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn invalid_cover_links_never_poison_anonymous_directory_results(db: PgPool) {
    let app = rookframe_community::router(db);
    for cover in [
        "https://",
        "https://user:password@example.com/image.png",
        "https://example.com/#fragment",
    ] {
        let path = format!("/worlds/{}/{}", Uuid::new_v4(), Uuid::new_v4());
        let mut data = listing();
        data["cover_image"] = json!(cover);
        let change = json!({"operation_id":Uuid::new_v4(),"expected_revision":0,"listing":data});
        assert_eq!(
            request(&app, "PUT", &path, change, Some(ADMIN)).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let (_, page) = request(&app, "GET", "/worlds", Value::Null, None).await;
    assert!(page["listings"].as_array().unwrap().is_empty());
}
