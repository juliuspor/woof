use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use axum::{
    body::{to_bytes, Body},
    http::{Method, Request, StatusCode},
    Router,
};
use chrono::{DateTime, Duration, Local, Timelike};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tower::ServiceExt;
use woof_core::{ApiToken, DataRetentionPolicy, WoofConfig, WoofPaths};
use woof_d::{
    GeneratedCompletion, GenerationRequest, MemoryClock, MemoryGenerationError, MemoryGenerator,
    MemoryScheduleConfig, MemoryScheduler,
};
use woof_llm::CancellationToken;
use woof_storage::{CaptureRecord, Storage, StorageRecoveryReason, TimeRuleWrite};

struct Fixture {
    app: Router,
    token: String,
    storage: Storage,
    state: woof_d::AppState,
    paths: WoofPaths,
    directory: PathBuf,
}

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn fixture() -> Fixture {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "woof-internal-{}-{unique}-{sequence}",
        std::process::id()
    ));
    let paths = WoofPaths::from_roots(directory.join(".woof"), directory.join("data"));
    let config = WoofConfig::for_paths(&paths);
    config.save(&paths.config_path).expect("save config");
    let storage = Storage::open(&paths.db_path).expect("storage");
    let token = "a".repeat(64);
    let api_token =
        ApiToken::parse_file(&paths.token_path, token.as_bytes().to_vec()).expect("token");
    let state = woof_d::AppState::new(storage.clone(), api_token)
        .with_persisted_config(paths.config_path.clone(), config);
    Fixture {
        app: woof_d::router(state.clone()),
        token,
        storage,
        state,
        paths,
        directory,
    }
}

async fn call(
    fixture: &Fixture,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", fixture.token));
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(serde_json::to_vec(&body).expect("encode body"))
    } else {
        Body::empty()
    };
    let response = fixture
        .app
        .clone()
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body");
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&bytes)}));
    (status, value)
}

#[tokio::test]
async fn capture_status_surfaces_safe_database_recovery_state() {
    let mut fixture = fixture();
    let state = fixture
        .state
        .clone()
        .with_database_recovery(Some(StorageRecoveryReason::Corrupt));
    fixture.app = woof_d::router(state.clone());
    fixture.state = state;

    let (status, value) = call(&fixture, Method::GET, "/capture/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        value["database_recovery"],
        json!({"occurred": true, "reason": "corrupt"})
    );
    let encoded = serde_json::to_string(&value).expect("status JSON");
    assert!(!encoded.contains(fixture.directory.to_string_lossy().as_ref()));

    let (status, accessibility) = call(&fixture, Method::GET, "/capture/accessibility", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(accessibility["trusted"].is_boolean());
    assert_eq!(accessibility["operational"], false);
    assert_eq!(accessibility["ready"], false);
    let _ = fs::remove_dir_all(fixture.directory);
}

fn seed_read_models(fixture: &Fixture) {
    let now = chrono::Utc::now().timestamp();
    let connection = fixture.storage.connect().expect("connection");
    connection
        .execute(
            "INSERT INTO chronicle
             (chronicle_id, level, period_key, summary_text, generated_at)
             VALUES ('chron-1', 'day', 'synthetic-day', 'Synthetic summary', ?1)",
            [now],
        )
        .expect("chronicle");
    connection
        .execute(
            "INSERT INTO salient_flags(kind, text, period_key, status, created_at)
             VALUES ('followup', 'Synthetic follow-up', 'synthetic-day', 'open', ?1)",
            [now],
        )
        .expect("follow-up");
    connection
        .execute(
            "INSERT INTO nudges
             (nudge_id, kind, scheduled_for, title, body, status, created_at)
             VALUES ('0194f3cb-16d8-7f10-a922-4379a7c54d31', 'contextual_nudge', ?1,
                     'Synthetic', 'Fixture', 'pending', ?1),
                    ('0194f3cb-16d8-7f10-a922-4379a7c54d32', 'contextual_nudge', ?1,
                     'Synthetic two', 'Fixture two', 'pending', ?1)",
            [now - 1],
        )
        .expect("nudge");
    connection
        .execute(
            "INSERT INTO kg_entities(entity_id, name, entity_type, first_seen, last_seen)
             VALUES ('entity-1', 'Alpha', 'project', ?1, ?1),
                    ('entity-2', 'Beta', 'person', ?1, ?1)",
            [now],
        )
        .expect("entities");
    connection
        .execute(
            "INSERT INTO kg_relations
             (relation_id, subject_id, predicate, object_id, valid_from)
             VALUES ('relation-1', 'entity-1', 'involves', 'entity-2', ?1)",
            [now],
        )
        .expect("relation");
    connection
        .execute(
            "INSERT INTO workflows
             (workflow_id, name, first_detected_at, last_detected_at, generated_at)
             VALUES ('0192f3cb-16d8-7f10-a922-4379a7c54d31', 'Synthetic workflow', ?1, ?1, ?1)",
            [now],
        )
        .expect("workflow");
    drop(connection);
    fixture
        .storage
        .record_capture(
            &CaptureRecord {
                snapshot_id: Some("snapshot-1".to_string()),
                content: "Synthetic activity".to_string(),
                app: "TextEdit".to_string(),
                window_title: "Fixture".to_string(),
                url: Some("https://example.test/fixture".to_string()),
                domain: Some("example.test".to_string()),
                captured_at: now,
                last_seen_at: now,
                duration_s: 30.0,
                focused_name: Some("Editor".to_string()),
                focused_role: Some("AXTextArea".to_string()),
                focused_path: Some("[\"Window\",\"Editor\"]".to_string()),
            },
            40,
        )
        .expect("capture");
}

#[tokio::test]
async fn read_models_return_seeded_database_state_and_nudges_transition() {
    let fixture = fixture();
    let (status, _) = call(
        &fixture,
        Method::POST,
        "/preferences/nudges-enabled",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    seed_read_models(&fixture);

    for (uri, key) in [
        ("/chronicle/followups", "followups"),
        ("/nudges/ready-unseen", "nudges"),
    ] {
        let (status, value) = call(&fixture, Method::GET, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {value}");
        assert!(!value[key].as_array().expect("array").is_empty(), "{uri}");
    }
    let (_, overview) = call(&fixture, Method::GET, "/stats/overview", None).await;
    assert_eq!(overview["overview"]["snapshots"], 1);
    let (_, work_patterns) = call(&fixture, Method::GET, "/work-patterns/status", None).await;
    assert_eq!(work_patterns["status"]["total"], 1);
    let (status, updated) = call(
        &fixture,
        Method::POST,
        "/work-patterns/update",
        Some(json!({
            "workflow_id": "0192f3cb-16d8-7f10-a922-4379a7c54d31",
            "status": "accepted"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["updated"], true);
    let (_, work_patterns) = call(&fixture, Method::GET, "/work-patterns/status", None).await;
    assert_eq!(work_patterns["status"]["recent"][0]["status"], "accepted");

    let (_, open_followups) = call(
        &fixture,
        Method::GET,
        "/chronicle/followups?status=open",
        None,
    )
    .await;
    let flag_id = open_followups["followups"][0]["flag_id"]
        .as_i64()
        .expect("follow-up ID");
    let (status, updated) = call(
        &fixture,
        Method::POST,
        "/chronicle/followups/status",
        Some(json!({"flag_id": flag_id, "status": "resolved"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["updated"], true);
    let (_, remaining) = call(
        &fixture,
        Method::GET,
        "/chronicle/followups?status=open",
        None,
    )
    .await;
    assert!(remaining["followups"].as_array().unwrap().is_empty());
    let (_, resolved) = call(
        &fixture,
        Method::GET,
        "/chronicle/followups?status=resolved",
        None,
    )
    .await;
    assert_eq!(resolved["followups"].as_array().unwrap().len(), 1);

    let (status, item) = call(
        &fixture,
        Method::GET,
        "/nudges/item?nudge_id=0194f3cb-16d8-7f10-a922-4379a7c54d31",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item["nudge"]["title"], "Synthetic");

    let (status, delivered) = call(
        &fixture,
        Method::POST,
        "/nudges/mark-delivered",
        Some(json!({"nudge_id": "0194f3cb-16d8-7f10-a922-4379a7c54d31"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(delivered["delivered"], true);
    let (_, still_ready) = call(&fixture, Method::GET, "/nudges/ready-unseen", None).await;
    assert!(still_ready["nudges"]
        .as_array()
        .unwrap()
        .iter()
        .any(|nudge| {
            nudge["nudge_id"] == "0194f3cb-16d8-7f10-a922-4379a7c54d31"
                && nudge["sent_at"].is_number()
        }));

    let (status, seen) = call(
        &fixture,
        Method::POST,
        "/nudges/mark-seen",
        Some(json!({"nudge_id": "0194f3cb-16d8-7f10-a922-4379a7c54d31"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen["seen"], true);
    let nudge_status: String = fixture
        .storage
        .connect()
        .expect("connection")
        .query_row(
            "SELECT status FROM nudges WHERE nudge_id='0194f3cb-16d8-7f10-a922-4379a7c54d31'",
            [],
            |row| row.get(0),
        )
        .expect("nudge status");
    assert_eq!(nudge_status, "seen");

    let (status, dismissed) = call(
        &fixture,
        Method::POST,
        "/nudges/dismiss",
        Some(json!({"nudge_id": "0194f3cb-16d8-7f10-a922-4379a7c54d32"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(dismissed["dismissed"], true);
    assert_eq!(
        fixture
            .storage
            .connect()
            .unwrap()
            .query_row(
                "SELECT status FROM nudges WHERE nudge_id='0194f3cb-16d8-7f10-a922-4379a7c54d32'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "dismissed"
    );
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn removed_http_surface_is_not_routable() {
    let fixture = fixture();
    let removed = [
        (Method::POST, "/time/rules", StatusCode::METHOD_NOT_ALLOWED),
        (Method::POST, "/time/rules/delete", StatusCode::NOT_FOUND),
        (Method::POST, "/time/assign-project", StatusCode::NOT_FOUND),
        (Method::GET, "/openai/status", StatusCode::NOT_FOUND),
        (Method::GET, "/chronicle/recent", StatusCode::NOT_FOUND),
        (Method::GET, "/wiki/graph", StatusCode::NOT_FOUND),
        (Method::GET, "/stats/focus", StatusCode::NOT_FOUND),
        (Method::GET, "/stats/places", StatusCode::NOT_FOUND),
        (Method::GET, "/stats/entities", StatusCode::NOT_FOUND),
        (Method::GET, "/style/notes", StatusCode::NOT_FOUND),
        (Method::POST, "/style/notes", StatusCode::NOT_FOUND),
        (Method::POST, "/style/replace", StatusCode::NOT_FOUND),
        (Method::POST, "/capture", StatusCode::NOT_FOUND),
        (
            Method::GET,
            "/capture/foreground-info",
            StatusCode::NOT_FOUND,
        ),
        (
            Method::GET,
            "/capture/frontmost-title",
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            "/preferences/onboarding-active",
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            "/request-ax-permission",
            StatusCode::NOT_FOUND,
        ),
        (Method::GET, "/ax-trusted", StatusCode::NOT_FOUND),
        (Method::POST, "/inline/type", StatusCode::NOT_FOUND),
        (Method::GET, "/inline/focus", StatusCode::NOT_FOUND),
        (Method::GET, "/inline/focus-rich", StatusCode::NOT_FOUND),
        (Method::POST, "/inline/wake-gmail", StatusCode::NOT_FOUND),
        (Method::GET, "/inline/focus-frame", StatusCode::NOT_FOUND),
        (Method::GET, "/inline/target-frame", StatusCode::NOT_FOUND),
        (
            Method::POST,
            "/inline/target-release",
            StatusCode::NOT_FOUND,
        ),
        (Method::POST, "/inline/deliver", StatusCode::NOT_FOUND),
        (Method::GET, "/inline/read", StatusCode::NOT_FOUND),
        (
            Method::POST,
            "/inline/recover-prompt",
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            "/inline/expand-gmail-quote",
            StatusCode::NOT_FOUND,
        ),
        (
            Method::POST,
            "/inline/paste-at-cursor",
            StatusCode::NOT_FOUND,
        ),
        (
            Method::GET,
            "/inline-rewrite/similar-outputs",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ];

    for (method, path, expected) in removed {
        let body = (method == Method::POST).then(|| json!({}));
        let (status, response) = call(&fixture, method, path, body).await;
        assert_eq!(status, expected, "{path}: {response}");
    }
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn retained_http_inputs_reject_noncanonical_unbounded_and_unknown_values() {
    let fixture = fixture();
    let oversized_query = format!("/search?q={}", "x".repeat(1_025));
    let oversized_slug = format!("/wiki/page?slug={}", "x".repeat(257));
    let oversized_snapshot_id = format!("/snapshots?ids={}", "x".repeat(129));
    let too_many_snapshot_ids = format!(
        "/snapshots?ids={}",
        (0..101)
            .map(|index| format!("snapshot-{index}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let invalid_queries = [
        "/search?q=dog&limti=2".to_string(),
        oversized_query,
        "/search?q=dog&limit=31".to_string(),
        oversized_slug,
        "/wiki/page?slug=Bad%20Slug".to_string(),
        oversized_snapshot_id,
        too_many_snapshot_ids,
        "/recent-activity?minutes=0".to_string(),
        "/recent-activity?limit=21".to_string(),
        "/working-memory?limit=201".to_string(),
        "/chronicle?level=day&period=synthetic-day".to_string(),
        "/wiki/list?type=unknown".to_string(),
        "/wiki/search?q=dog&limit=101".to_string(),
        "/chronicle/followups?status=unknown".to_string(),
        "/nudges/item?nudge_id=not-a-uuid".to_string(),
        "/nudges/item?nudge_id=0194f3cb-16d8-7f10-a922-4379a7c54d31&extra=1".to_string(),
        "/nudges/ready-unseen?limit=0".to_string(),
        "/stats/overview?minutes=0".to_string(),
        "/work-patterns/status?limit=101".to_string(),
        "/time/report?period=unknown".to_string(),
        "/time/report?period=today&from=2030-01-01".to_string(),
    ];
    for uri in invalid_queries {
        let (status, response) = call(&fixture, Method::GET, &uri, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {response}");
    }

    let (status, _) = call(
        &fixture,
        Method::GET,
        "/chronicle?level=day&period=2030-01-15",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let now = chrono::Utc::now().timestamp();
    let invalid_bodies = [
        (
            "/nudges/mark-seen",
            json!({"nudge_id":"invalid/id"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/nudges/mark-delivered",
            json!({"nudge_id":"not-a-uuid"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/nudges/dismiss",
            json!({
                "nudge_id":"0194f3cb-16d8-7f10-a922-4379a7c54d31",
                "unexpected":true
            }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/rules/delete",
            json!({"rule_id":"invalid/id"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/chronicle/followups/status",
            json!({"flag_id": 0, "status":"resolved"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/chronicle/followups/status",
            json!({"flag_id": 1, "status":"open"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/chronicle/followups/status",
            json!({"flag_id": 1, "status":"dismissed", "unexpected":true}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/work-patterns/update",
            json!({"workflow_id":"not-a-uuid", "status":"accepted"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/work-patterns/update",
            json!({
                "workflow_id":"0192f3cb-16d8-7f10-a922-4379a7c54d31",
                "status":"proposed"
            }),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/work-patterns/update",
            json!({
                "workflow_id":"0192f3cb-16d8-7f10-a922-4379a7c54d31",
                "status":"dismissed",
                "unexpected":true
            }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/identity/set-name",
            json!({"name":"Bad\nName"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/identity/set-name",
            json!({"name":"Boxer","unexpected":true}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/chat/record",
            json!({"thread_id":"thread-1","role":"system","content":"hello"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/chat/record",
            json!({"thread_id":"invalid/id","role":"user","content":"hello"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/chat/record",
            json!({"thread_id":"thread-1","role":"user","content":"bad\u{0}text"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/chat/record",
            json!({"thread_id":"thread-1","role":"user","content":"hello","created_at":now + 86_401}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/chat/record",
            json!({"thread_id":"thread-1","role":"user","content":"hello","unexpected":true}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/inline-rewrite/record",
            json!({"app":"","domain":""}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/inline-rewrite/record",
            json!({"app":"Mail","domain":"https://example.test"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/inline-rewrite/record",
            json!({"app":"Mail","domain":"example.test","used_at":now + 86_401}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/inline-rewrite/record-output",
            json!({"app":"Mail","domain":"example.test","instruction":"shorten","output":"fixture","created_at":-1}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/inline-rewrite/record-output",
            json!({"app":"Mail","domain":"example.test","instruction":"shorten","output":"x".repeat(256 * 1_024 + 1)}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/inline-rewrite/similar-outputs",
            json!({"app":"Mail","domain":"example.test","instruction":"x".repeat(4 * 1_024 + 1)}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/preferences/nudges-enabled",
            json!({"enabled":true,"unexpected":false}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/capture/blacklist",
            json!({"blacklist":[{"kind":"app_name","pattern":"Mail","unexpected":true}]}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "/rules",
            json!({
                "label":"Fixture",
                "prompt":"Fixture",
                "schedule_kind":"once",
                "timezone":"local",
                "fire_at":now + 60,
                "unexpected":true
            }),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ];
    for (path, body, expected) in invalid_bodies {
        let (status, response) = call(&fixture, Method::POST, path, Some(body)).await;
        assert_eq!(status, expected, "{path}: {response}");
    }

    let (status, response) = call(
        &fixture,
        Method::PUT,
        "/data/retention",
        Some(json!({"mode":"keep_forever","days":1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn rules_chat_and_inline_record_routes_mutate_and_read_back() {
    let fixture = fixture();
    let (timezone_status, _) = call(
        &fixture,
        Method::POST,
        "/rules",
        Some(json!({
            "label": "Unsupported timezone",
            "prompt": "Review the fixture",
            "schedule_kind": "daily",
            "hour": 10,
            "minute": 15,
            "timezone": "UTC"
        })),
    )
    .await;
    assert_eq!(timezone_status, StatusCode::BAD_REQUEST);

    let (status, created) = call(
        &fixture,
        Method::POST,
        "/rules",
        Some(json!({
            "label": "Synthetic reminder",
            "prompt": "Review the fixture",
            "schedule_kind": "daily",
            "hour": 10,
            "minute": 15,
            "timezone": "local"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let rule_id = created["rule"]["rule_id"]
        .as_str()
        .expect("rule id")
        .to_string();
    let (_, listed) = call(&fixture, Method::GET, "/rules", None).await;
    assert_eq!(listed["rules"].as_array().expect("rules").len(), 1);

    let (_, chat) = call(
        &fixture,
        Method::POST,
        "/chat/record",
        Some(json!({"thread_id":"thread-1","role":"user","content":"Synthetic turn"})),
    )
    .await;
    assert!(chat["turn_id"].as_i64().expect("turn id") > 0);

    for _ in 0..2 {
        let (_, use_result) = call(
            &fixture,
            Method::POST,
            "/inline-rewrite/record",
            Some(json!({"app":"Mail","domain":"example.test"})),
        )
        .await;
        assert!(use_result["use_count"].as_i64().expect("use count") >= 1);
    }
    let (_, output) = call(
        &fixture,
        Method::POST,
        "/inline-rewrite/record-output",
        Some(json!({
            "app":"Mail",
            "domain":"example.test",
            "instruction":"shorten",
            "output":"Synthetic output"
        })),
    )
    .await;
    assert_eq!(output["output"]["output"], "Synthetic output");
    let (_, similar) = call(
        &fixture,
        Method::POST,
        "/inline-rewrite/similar-outputs",
        Some(json!({
            "app":"Mail",
            "domain":"example.test",
            "instruction":"shorten"
        })),
    )
    .await;
    assert_eq!(similar["outputs"].as_array().expect("outputs").len(), 1);

    let (_, deleted) = call(
        &fixture,
        Method::POST,
        "/rules/delete",
        Some(json!({"rule_id": rule_id})),
    )
    .await;
    assert_eq!(deleted["deleted"], true);
    let count: i64 = fixture
        .storage
        .connect()
        .expect("connection")
        .query_row("SELECT count(*) FROM chat_turns", [], |row| row.get(0))
        .expect("chat count");
    assert_eq!(count, 1);
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn reminder_api_rejects_noncanonical_or_unbounded_schedules() {
    let fixture = fixture();
    let now = chrono::Utc::now().timestamp();
    let invalid_requests = [
        json!({
            "label": "Missing clock",
            "prompt": "Fixture",
            "schedule_kind": "daily",
            "timezone": "local"
        }),
        json!({
            "label": "Bad weekdays",
            "prompt": "Fixture",
            "schedule_kind": "weekly",
            "days_of_week": [3, 2],
            "hour": 9,
            "minute": 0,
            "timezone": "local"
        }),
        json!({
            "label": "Short interval",
            "prompt": "Fixture",
            "schedule_kind": "interval",
            "interval_minutes": 4,
            "timezone": "local"
        }),
        json!({
            "label": "Past reminder",
            "prompt": "Fixture",
            "schedule_kind": "once",
            "fire_at": now - 1,
            "timezone": "local"
        }),
        json!({
            "label": "Control character",
            "prompt": "first line\nsecond line",
            "schedule_kind": "daily",
            "hour": 9,
            "minute": 0,
            "timezone": "local"
        }),
        json!({
            "label": "x".repeat(121),
            "prompt": "Fixture",
            "schedule_kind": "once",
            "fire_at": now + 60,
            "timezone": "local"
        }),
    ];
    for request in invalid_requests {
        let (status, _) = call(&fixture, Method::POST, "/rules", Some(request)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    let (status, weekly) = call(
        &fixture,
        Method::POST,
        "/rules",
        Some(json!({
            "label": "Canonical weekly reminder",
            "prompt": "Review the fixture",
            "schedule_kind": "weekly",
            "days_of_week": [1, 3, 7],
            "hour": 9,
            "minute": 30,
            "timezone": "local"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{weekly}");
    assert_eq!(weekly["rule"]["days_of_week"], json!([1, 3, 7]));
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn sensitive_http_mutations_are_redacted_and_blacklists_are_validated() {
    let fixture = fixture();
    let private = "Visa 4111 1111 1111 1111; CVV: 123; SSN 123-45-6789";

    let (status, _) = call(
        &fixture,
        Method::POST,
        "/chat/record",
        Some(json!({"thread_id":"private","role":"user","content":private})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let stored_chat: String = fixture
        .storage
        .connect()
        .expect("connection")
        .query_row(
            "SELECT content FROM chat_turns WHERE thread_id='private'",
            [],
            |row| row.get(0),
        )
        .expect("chat");
    assert!(!stored_chat.contains("4111 1111 1111 1111"));
    assert!(stored_chat.contains("[REDACTED_CARD]"));
    assert!(stored_chat.contains("[REDACTED_CVV]"));
    assert!(stored_chat.contains("[REDACTED_SSN]"));

    let (status, output) = call(
        &fixture,
        Method::POST,
        "/inline-rewrite/record-output",
        Some(json!({
            "app":"Mail",
            "domain":"example.test",
            "instruction":private,
            "output":private
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(output["output"]["instruction"], stored_chat);
    assert_eq!(output["output"]["output"], stored_chat);

    let (status, normalized) = call(
        &fixture,
        Method::POST,
        "/capture/blacklist",
        Some(json!({"blacklist":[{"kind":"regex","pattern":"private-[0-9]+"}]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(normalized["blacklist"][0]["kind"], "regex");
    assert_eq!(normalized["blacklist"][0]["pattern"], "private-[0-9]+");

    for pattern in ["example.com.", "[2001:db8::1]", "2001:0db8:0:0:0:0:0:1"] {
        let (status, response) = call(
            &fixture,
            Method::POST,
            "/capture/blacklist",
            Some(json!({"blacklist":[{"kind":"browser_host","pattern":pattern}]})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{pattern}: {response}");
    }

    for blacklist in [
        json!([{"kind":" REGEX ","pattern":"  private-[0-9]+  "}]),
        json!([{"kind":"regex","pattern":"("}]),
        json!([{"kind":"unknown","pattern":"private"}]),
        json!([{"kind":"app_name","pattern":"  "}]),
        json!([{"kind":"browser_host","pattern":"https://example.com"}]),
        json!([{"kind":"browser_host","pattern":"example.com/private"}]),
        json!([{"kind":"browser_host","pattern":"user@example.com"}]),
        json!([{"kind":"browser_host","pattern":"example.com:443"}]),
        json!([{"kind":"browser_host","pattern":"[2001:db8::1]:443"}]),
    ] {
        let (status, _) = call(
            &fixture,
            Method::POST,
            "/capture/blacklist",
            Some(json!({"blacklist": blacklist})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn generated_time_rules_have_a_read_only_http_surface() {
    let fixture = fixture();
    fixture
        .storage
        .save_time_rule(
            None,
            &TimeRuleWrite {
                project: "Generated project".to_string(),
                app: Some("Code".to_string()),
                domain: None,
                title_contains: Some("fixture".to_string()),
                source: "generated".to_string(),
                created_at: chrono::Utc::now().timestamp(),
            },
        )
        .expect("save generated rule");

    let (status, listed) = call(&fixture, Method::GET, "/time/rules", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["rules"][0]["project"], "Generated project");

    let (status, _) = call(
        &fixture,
        Method::POST,
        "/time/rules",
        Some(json!({"project": "Rejected"})),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = call(
        &fixture,
        Method::POST,
        "/time/assign-project",
        Some(json!({"project": "Rejected", "snapshot_ids": ["snapshot-1"]})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = fs::remove_dir_all(&fixture.directory);
}

#[tokio::test]
async fn notification_preference_has_one_private_config_source_of_truth() {
    let fixture = fixture();
    let (_, enabled) = call(
        &fixture,
        Method::POST,
        "/preferences/nudges-enabled",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(enabled["enabled"], true);
    let saved = WoofConfig::load_or_create(&fixture.paths).expect("enabled config");
    assert!(saved.nudges_enabled);
    let (_, nudges) = call(
        &fixture,
        Method::POST,
        "/preferences/nudges-enabled",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(nudges["enabled"], false);
    let saved = WoofConfig::load_or_create(&fixture.paths).expect("disabled config");
    assert!(!saved.nudges_enabled);
    let preferences_path = fixture.paths.config_dir.join("preferences.json");
    assert!(!preferences_path.exists());
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn retention_policy_is_authenticated_persisted_and_enforced_immediately() {
    let fixture = fixture();
    let now = chrono::Utc::now().timestamp();
    for (snapshot_id, observed_at) in [
        ("expired-retention", now - 40 * 86_400),
        ("current-retention", now),
    ] {
        fixture
            .storage
            .record_capture(
                &CaptureRecord {
                    snapshot_id: Some(snapshot_id.to_string()),
                    content: format!("Synthetic {snapshot_id}"),
                    app: "TextEdit".to_string(),
                    window_title: "Retention fixture".to_string(),
                    url: None,
                    domain: None,
                    captured_at: observed_at,
                    last_seen_at: observed_at,
                    duration_s: 0.0,
                    focused_name: None,
                    focused_role: None,
                    focused_path: None,
                },
                20,
            )
            .expect("seed capture");
    }

    let (status, initial) = call(&fixture, Method::GET, "/data/retention", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(initial["retention"]["mode"], "keep_forever");

    let (status, updated) = call(
        &fixture,
        Method::PUT,
        "/data/retention",
        Some(json!({"mode": "days", "days": 30})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["retention"], json!({"mode": "days", "days": 30}));
    assert_eq!(updated["pruned"]["expired_snapshots"], 1);
    assert!(fixture
        .storage
        .snapshots(&["expired-retention".to_string()])
        .expect("expired capture")
        .is_empty());
    assert_eq!(
        fixture
            .storage
            .snapshots(&["current-retention".to_string()])
            .expect("current capture")
            .len(),
        1
    );
    assert_eq!(
        WoofConfig::load_or_create(&fixture.paths)
            .expect("saved config")
            .data_retention,
        DataRetentionPolicy::Days { days: 30 }
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&fixture.paths.config_path)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let (invalid_status, _) = call(
        &fixture,
        Method::PUT,
        "/data/retention",
        Some(json!({"mode": "days", "days": 0})),
    )
    .await;
    assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn capture_and_due_rules_generate_runtime_rows_without_manual_seeding() {
    let fixture = fixture();
    let (status, _) = call(
        &fixture,
        Method::POST,
        "/preferences/nudges-enabled",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let now = chrono::Utc::now().timestamp();
    for capture in [
        CaptureRecord {
            snapshot_id: Some("generated-a".to_string()),
            content: "Synthetic recurring Atlas planning".to_string(),
            app: "TextEdit".to_string(),
            window_title: "Atlas planning".to_string(),
            url: Some("https://example.test/atlas".to_string()),
            domain: Some("example.test".to_string()),
            captured_at: now - 7_200,
            last_seen_at: now - 7_080,
            duration_s: 120.0,
            focused_name: Some("Editor".to_string()),
            focused_role: Some("AXTextArea".to_string()),
            focused_path: Some("[\"Window\",\"Editor\"]".to_string()),
        },
        CaptureRecord {
            snapshot_id: Some("generated-b".to_string()),
            content: "Synthetic recurring Atlas planning follow-up".to_string(),
            app: "TextEdit".to_string(),
            window_title: "Atlas planning".to_string(),
            url: Some("https://example.test/atlas".to_string()),
            domain: Some("example.test".to_string()),
            captured_at: now - 3_600,
            last_seen_at: now - 3_480,
            duration_s: 120.0,
            focused_name: Some("Editor".to_string()),
            focused_role: Some("AXTextArea".to_string()),
            focused_path: Some("[\"Window\",\"Editor\"]".to_string()),
        },
        CaptureRecord {
            snapshot_id: Some("generated-c".to_string()),
            content: "Synthetic recurring Atlas planning final pass".to_string(),
            app: "TextEdit".to_string(),
            window_title: "Atlas planning".to_string(),
            url: Some("https://example.test/atlas".to_string()),
            domain: Some("example.test".to_string()),
            captured_at: now - 120,
            last_seen_at: now - 1,
            duration_s: 119.0,
            focused_name: Some("Editor".to_string()),
            focused_role: Some("AXTextArea".to_string()),
            focused_path: Some("[\"Window\",\"Editor\"]".to_string()),
        },
    ] {
        fixture
            .storage
            .record_capture(&capture, 200)
            .expect("record capture");
    }

    let connection = fixture.storage.connect().expect("connection");
    let ledger_seconds: f64 = connection
        .query_row("SELECT sum(seconds) FROM time_ledger", [], |row| row.get(0))
        .expect("ledger");
    assert_eq!(ledger_seconds, 359.0);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM workflows", [], |row| row
                .get::<_, i64>(0))
            .expect("workflows"),
        1
    );
    drop(connection);

    let (_, patterns) = call(&fixture, Method::GET, "/work-patterns/status", None).await;
    assert_eq!(patterns["status"]["total"], 1);
    let (_, generated_nudges) = call(&fixture, Method::GET, "/nudges/ready-unseen", None).await;
    assert!(generated_nudges["nudges"]
        .as_array()
        .expect("generated nudges")
        .iter()
        .any(|nudge| nudge["dedupe_key"]
            .as_str()
            .is_some_and(|key| key.starts_with("workflow:"))));

    let scheduled_for = chrono::Utc::now().timestamp() + 2;
    let (status, rule) = call(
        &fixture,
        Method::POST,
        "/rules",
        Some(json!({
            "label": "Review Atlas",
            "prompt": "Open the Atlas follow-up",
            "schedule_kind": "once",
            "timezone": "local",
            "fire_at": scheduled_for
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rule}");
    let rule_id = rule["rule"]["rule_id"].as_str().expect("rule id");

    let automation = woof_d::spawn_automation_service(
        fixture.state.clone(),
        std::time::Duration::from_millis(5),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let ready: i64 = fixture
                .storage
                .connect()
                .expect("connection")
                .query_row(
                    "SELECT count(*) FROM nudges WHERE kind='scheduled_rule' AND status='ready'",
                    [],
                    |row| row.get(0),
                )
                .expect("ready scheduled nudges");
            if ready > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("automation scheduler made the nudge notification-ready");
    automation.shutdown().await;

    let (_, scheduled) = call(&fixture, Method::GET, "/nudges/ready-unseen", None).await;
    assert!(scheduled["nudges"]
        .as_array()
        .expect("scheduled nudges")
        .iter()
        .any(|nudge| nudge["kind"] == "scheduled_rule"));
    let fired_at: Option<i64> = fixture
        .storage
        .connect()
        .expect("connection")
        .query_row(
            "SELECT last_fired_at FROM proactive_rules WHERE rule_id=?1",
            [rule_id],
            |row| row.get(0),
        )
        .expect("last fired");
    assert_eq!(fired_at, Some(scheduled_for));
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn delete_all_data_clears_memory_and_identity_but_preserves_the_service() {
    let fixture = fixture();
    let now = chrono::Utc::now().timestamp();
    fixture
        .storage
        .record_capture(
            &CaptureRecord {
                snapshot_id: Some("delete-me".to_string()),
                content: "Synthetic private memory".to_string(),
                app: "TextEdit".to_string(),
                window_title: "Private draft".to_string(),
                url: None,
                domain: None,
                captured_at: now,
                last_seen_at: now,
                duration_s: 0.0,
                focused_name: None,
                focused_role: None,
                focused_path: None,
            },
            200,
        )
        .expect("record capture");
    let (identity_status, identity) = call(
        &fixture,
        Method::POST,
        "/identity/set-name",
        Some(json!({"name": "Synthetic Person"})),
    )
    .await;
    assert_eq!(identity_status, StatusCode::OK, "{identity}");

    let (status, response) = call(&fixture, Method::POST, "/data/delete-all", None).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["status"], "deleted");
    assert!(response["deleted_rows"]
        .as_u64()
        .is_some_and(|rows| rows > 0));
    assert_eq!(response["vector_index"]["indexed"], 0);

    let (activity_status, activity) = call(&fixture, Method::GET, "/recent-activity", None).await;
    assert_eq!(activity_status, StatusCode::OK);
    assert_eq!(activity["activity"], json!([]));
    let (identity_status, identity) = call(&fixture, Method::GET, "/identity", None).await;
    assert_eq!(identity_status, StatusCode::OK);
    assert!(identity["identity"]["name"].is_null());
    assert_eq!(
        fs::read_to_string(&fixture.paths.identity_path).expect("cleared identity"),
        "{}\n"
    );
    let (health_status, health) = call(&fixture, Method::GET, "/health", None).await;
    assert_eq!(health_status, StatusCode::OK);
    assert_eq!(health["status"], "ok");
    let _ = fs::remove_dir_all(fixture.directory);
}

#[derive(Clone)]
struct ResetTestClock {
    now: DateTime<Local>,
}

impl MemoryClock for ResetTestClock {
    fn now(&self) -> DateTime<Local> {
        self.now
    }
}

struct ResetBlockingGenerator {
    prompts: Mutex<Vec<String>>,
    started: Arc<Notify>,
    cancellation_observed: Arc<Notify>,
    release_after_cancellation: Arc<Notify>,
}

#[async_trait::async_trait]
impl MemoryGenerator for ResetBlockingGenerator {
    async fn generate(
        &self,
        request: GenerationRequest,
        cancellation: &CancellationToken,
    ) -> Result<GeneratedCompletion, MemoryGenerationError> {
        self.prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.prompt);
        self.started.notify_one();
        cancellation.cancelled().await;
        self.cancellation_observed.notify_one();
        self.release_after_cancellation.notified().await;
        Err(MemoryGenerationError::Cancelled)
    }
}

#[tokio::test]
async fn delete_all_cancels_and_quiesces_pre_reset_memory_generation() {
    let fixture = fixture();
    let now = Local::now();
    let hour_start = now
        .with_minute(0)
        .and_then(|value| value.with_second(0))
        .and_then(|value| value.with_nanosecond(0))
        .expect("valid local hour")
        - Duration::hours(1);
    fixture
        .storage
        .record_capture(
            &CaptureRecord {
                snapshot_id: Some("memory-reset-private".to_string()),
                content: "Private text retained by the active prompt".to_string(),
                app: "TextEdit".to_string(),
                window_title: "Private reset fixture".to_string(),
                url: None,
                domain: None,
                captured_at: (hour_start + Duration::minutes(1)).timestamp(),
                last_seen_at: (hour_start + Duration::minutes(1)).timestamp(),
                duration_s: 0.0,
                focused_name: None,
                focused_role: None,
                focused_path: None,
            },
            20,
        )
        .expect("seed pre-reset capture");

    let started = Arc::new(Notify::new());
    let cancellation_observed = Arc::new(Notify::new());
    let release_after_cancellation = Arc::new(Notify::new());
    let generator = Arc::new(ResetBlockingGenerator {
        prompts: Mutex::new(Vec::new()),
        started: started.clone(),
        cancellation_observed: cancellation_observed.clone(),
        release_after_cancellation: release_after_cancellation.clone(),
    });
    let scheduler = Arc::new(
        MemoryScheduler::new(
            fixture.storage.clone(),
            generator.clone(),
            Arc::new(ResetTestClock { now }),
            MemoryScheduleConfig {
                poll_interval: std::time::Duration::from_secs(300),
                hour_backfill: 1,
                day_backfill: 0,
                week_backfill: 0,
                month_backfill: 0,
                year_backfill: 0,
            },
        )
        .with_storage_mutation_barrier(fixture.state.storage_mutation_barrier())
        .with_generation_gate(fixture.state.memory_generation_gate()),
    );
    let active_scheduler = scheduler.clone();
    let active_run = tokio::spawn(async move {
        active_scheduler
            .run_due_once(&CancellationToken::new())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .expect("pre-reset generation started");

    let delete_app = fixture.app.clone();
    let delete_token = fixture.token.clone();
    let delete_task = tokio::spawn(async move {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/data/delete-all")
            .header("authorization", format!("Bearer {delete_token}"))
            .body(Body::empty())
            .expect("delete request");
        let response = delete_app.oneshot(request).await.expect("delete response");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("delete response body");
        (
            status,
            serde_json::from_slice::<Value>(&body).expect("delete JSON"),
        )
    });

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        cancellation_observed.notified(),
    )
    .await
    .expect("delete-all cancelled the active transport");
    assert!(
        !delete_task.is_finished(),
        "delete-all must wait until the cancelled run releases its pre-reset prompt"
    );
    assert_eq!(
        fixture
            .storage
            .snapshots(&["memory-reset-private".to_string()])
            .expect("snapshot before quiescence")
            .len(),
        1,
        "durable deletion must begin only after generation is quiescent"
    );

    release_after_cancellation.notify_one();
    let active_report = tokio::time::timeout(std::time::Duration::from_secs(2), active_run)
        .await
        .expect("cancelled run stopped")
        .expect("cancelled run joined");
    assert!(active_report.cancelled);
    let (delete_status, delete_body) =
        tokio::time::timeout(std::time::Duration::from_secs(2), delete_task)
            .await
            .expect("delete-all completed after quiescence")
            .expect("delete task joined");
    assert_eq!(delete_status, StatusCode::OK, "{delete_body}");
    assert!(fixture
        .storage
        .snapshots(&["memory-reset-private".to_string()])
        .expect("snapshots after reset")
        .is_empty());
    assert_eq!(
        generator
            .prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1,
        "no later prompt may be sent from the pre-reset run"
    );

    let later_report = scheduler.run_due_once(&CancellationToken::new()).await;
    assert!(!later_report.cancelled, "the reset lease must be released");
    assert_eq!(
        generator
            .prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1,
        "the resumed scheduler must observe empty post-reset storage"
    );
    let _ = fs::remove_dir_all(fixture.directory);
}
