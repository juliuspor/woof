use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
};
use tower::ServiceExt;
use woof_core::{health_proof, ApiToken, HEALTH_CHALLENGE_HEADER, HEALTH_PROOF_HEADER};
use woof_storage::Storage;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn fixture() -> (axum::Router, String, PathBuf) {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "woof-http-{}-{unique}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("temporary directory");
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    let token_value = "a".repeat(64);
    let token = ApiToken::parse_file(
        &directory.join("api-token"),
        token_value.as_bytes().to_vec(),
    )
    .expect("token");
    (
        woof_d::router(woof_d::AppState::new(storage, token)),
        token_value,
        directory,
    )
}

async fn body(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body")
            .to_vec(),
    )
    .expect("UTF-8")
}

#[tokio::test]
async fn health_is_public_and_exact() {
    let (app, _, directory) = fixture();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, r#"{"status":"ok"}"#);
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn valid_health_challenges_receive_a_token_bound_proof() {
    let (app, token_value, directory) = fixture();
    let challenge = "01".repeat(32);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(HEALTH_CHALLENGE_HEADER, &challenge)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let token = ApiToken::parse_file(&directory.join("expected-token"), token_value.into_bytes())
        .expect("expected token");
    assert_eq!(
        response
            .headers()
            .get(HEALTH_PROOF_HEADER)
            .and_then(|value| value.to_str().ok()),
        health_proof(&token, &challenge).as_deref()
    );
    assert_eq!(body(response).await, r#"{"status":"ok"}"#);

    let malformed = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(HEALTH_CHALLENGE_HEADER, "malformed")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert!(malformed.headers().get(HEALTH_PROOF_HEADER).is_none());
    assert_eq!(body(malformed).await, r#"{"status":"ok"}"#);
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn only_exact_get_health_bypasses_authentication() {
    let (app, _, directory) = fixture();
    for (method, uri) in [
        (Method::POST, "/health"),
        (Method::PUT, "/health"),
        (Method::GET, "/health/"),
        (Method::GET, "/health-check"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(body(response).await, r#"{"error":"Unauthorized"}"#);
    }
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn authentication_runs_before_known_and_unknown_protected_routes() {
    let (app, token, directory) = fixture();
    for uri in [
        "/working-memory",
        "/capture/accessibility",
        "/time/assign-project",
        "/capture/foreground-info",
        "/ax-trusted",
        "/inline/read",
        "/nudges/item?nudge_id=0194f3cb-16d8-7f10-a922-4379a7c54d31",
        "/nudges/ready-unseen",
        "/rules",
        "/data/retention",
        "/data/delete-all",
        "/does-not-exist",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body(response).await, r#"{"error":"Unauthorized"}"#);
    }

    let prompt = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/capture/accessibility/request")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(prompt.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body(prompt).await, r#"{"error":"Unauthorized"}"#);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/does-not-exist")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body(response).await, r#"{"error":"Not Found"}"#);
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn valid_bearer_token_unlocks_routes_and_bad_lengths_do_not() {
    let (app, token, directory) = fixture();
    let bad = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/working-memory")
                .header("authorization", "Bearer short")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

    let valid = app
        .oneshot(
            Request::builder()
                .uri("/working-memory?limit=40")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(valid.status(), StatusCode::OK);
    assert_eq!(body(valid).await, r#"{"items":[]}"#);
    let _ = fs::remove_dir_all(directory);
}
