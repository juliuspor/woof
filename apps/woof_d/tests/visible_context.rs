use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use tower::ServiceExt;
use woof_capture::{
    capture_contextual_reply_after_surface_preflight, validate_capture_target, AccessibilityNode,
    AccessibilityProvider, AccessibilityRect, CaptureError, CapturePolicy, ForegroundCapture,
    RawCapture,
};
use woof_core::{ApiToken, CaptureBlacklistEntry};
use woof_storage::Storage;

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
enum ProviderOutcome {
    Captured(Box<RawCapture>),
    Blacklisted,
    Unavailable,
}

struct FixedProvider {
    calls: Arc<AtomicUsize>,
    tree_reads: Arc<AtomicUsize>,
    outcome: ProviderOutcome,
}

#[async_trait]
impl AccessibilityProvider for FixedProvider {
    async fn capture_foreground(
        &self,
        _policy: &CapturePolicy,
    ) -> Result<ForegroundCapture, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.tree_reads.fetch_add(1, Ordering::SeqCst);
        match &self.outcome {
            ProviderOutcome::Captured(capture) => Ok(ForegroundCapture::Captured(capture.clone())),
            ProviderOutcome::Blacklisted => Ok(ForegroundCapture::Blacklisted),
            ProviderOutcome::Unavailable => Err(CaptureError::NoFocusedApplication),
        }
    }

    async fn capture_foreground_for_target(
        &self,
        policy: &CapturePolicy,
        expected_pid: i32,
        expected_window_title: &str,
        expected_window_id: Option<i64>,
    ) -> Result<ForegroundCapture, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.outcome {
            ProviderOutcome::Captured(capture) => {
                validate_capture_target(
                    capture.pid,
                    capture.window_title.as_deref(),
                    capture.window_id,
                    expected_pid,
                    expected_window_title,
                    expected_window_id,
                )?;
                if policy.is_blacklisted(capture) {
                    return Ok(ForegroundCapture::Blacklisted);
                }
                let browser_urls = capture.browser_url.as_slice();
                capture_contextual_reply_after_surface_preflight(
                    capture.bundle_id.as_deref(),
                    browser_urls,
                    || {
                        self.tree_reads.fetch_add(1, Ordering::SeqCst);
                        Ok(ForegroundCapture::Captured(capture.clone()))
                    },
                )
            }
            ProviderOutcome::Blacklisted => Ok(ForegroundCapture::Blacklisted),
            ProviderOutcome::Unavailable => Err(CaptureError::NoFocusedApplication),
        }
    }
}

struct Fixture {
    app: Router,
    token: String,
    storage: Storage,
    state: woof_d::AppState,
    calls: Arc<AtomicUsize>,
    tree_reads: Arc<AtomicUsize>,
    directory: PathBuf,
}

fn fixture(capture: RawCapture, blacklist: Vec<CaptureBlacklistEntry>) -> Fixture {
    fixture_with_outcome(ProviderOutcome::Captured(Box::new(capture)), blacklist)
}

fn fixture_with_outcome(
    outcome: ProviderOutcome,
    blacklist: Vec<CaptureBlacklistEntry>,
) -> Fixture {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "woof-visible-context-{}-{unique}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("temporary directory");
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    let token = "a".repeat(64);
    let api_token = ApiToken::parse_file(&directory.join("api-token"), token.as_bytes().to_vec())
        .expect("token");
    let calls = Arc::new(AtomicUsize::new(0));
    let tree_reads = Arc::new(AtomicUsize::new(0));
    let provider = FixedProvider {
        calls: calls.clone(),
        tree_reads: tree_reads.clone(),
        outcome,
    };
    let state = woof_d::AppState::new(storage.clone(), api_token)
        .with_initial_blacklist(blacklist)
        .with_visible_context_provider(provider);
    Fixture {
        app: woof_d::router(state.clone()),
        token,
        storage,
        state,
        calls,
        tree_reads,
        directory,
    }
}

fn rect(x: i64, y: i64, width: i64, height: i64) -> AccessibilityRect {
    AccessibilityRect {
        x,
        y,
        width,
        height,
    }
}

fn capture_with_context(text: &str) -> RawCapture {
    RawCapture {
        captured_at_ms: 1_000,
        pid: 42,
        app_name: "Slack".to_string(),
        bundle_id: Some("com.tinyspeck.slackmacgap".to_string()),
        window_title: Some("Roadmap — Slack".to_string()),
        window_id: Some(9_001),
        browser_url: Some("https://Example.COM/client/workspace/channel".to_string()),
        secure_input: false,
        root: AccessibilityNode {
            role: "AXWindow".to_string(),
            frame: Some(rect(0, 0, 1_000, 800)),
            children: vec![AccessibilityNode {
                role: "AXGroup".to_string(),
                frame: Some(rect(300, 100, 650, 650)),
                children: vec![
                    AccessibilityNode {
                        role: "AXStaticText".to_string(),
                        frame: Some(rect(340, 600, 400, 30)),
                        value: Some(text.to_string()),
                        ..AccessibilityNode::default()
                    },
                    AccessibilityNode {
                        role: "AXTextArea".to_string(),
                        frame: Some(rect(300, 700, 650, 50)),
                        value: Some(String::new()),
                        placeholder: Some("Message #roadmap".to_string()),
                        focused: true,
                        ..AccessibilityNode::default()
                    },
                ],
                ..AccessibilityNode::default()
            }],
            ..AccessibilityNode::default()
        },
    }
}

fn whatsapp_capture_with_context(text: &str) -> RawCapture {
    let mut capture = capture_with_context(text);
    capture.app_name = "Google Chrome".to_string();
    capture.bundle_id = Some("com.google.Chrome".to_string());
    capture.browser_url = Some("https://web.whatsapp.com/chat".to_string());
    let mut conversation = capture.root.children.remove(0);
    conversation.children[1].placeholder = Some("Type a message".to_string());
    capture.root.children.push(AccessibilityNode {
        role: "AXWebArea".to_string(),
        frame: Some(rect(0, 0, 1_000, 800)),
        url: Some("https://web.whatsapp.com/chat".to_string()),
        children: vec![conversation],
        ..AccessibilityNode::default()
    });
    capture
}

fn request_body() -> Value {
    json!({
        "expected_pid": 42,
        "expected_window_title": "Roadmap — Slack",
        "expected_window_id": 9_001
    })
}

async fn call(fixture: &Fixture, token: Option<&str>, request_body: Value) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/inline-rewrite/visible-context")
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = fixture
        .app
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    serde_json::to_vec(&request_body).expect("encode body"),
                ))
                .expect("request"),
        )
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
async fn visible_context_requires_authentication_before_capture() {
    let fixture = fixture(capture_with_context("Recent project update"), vec![]);

    let (status, value) = call(&fixture, None, request_body()).await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(value, json!({"error": "Unauthorized"}));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_respects_capture_pause_without_reading_accessibility() {
    let fixture = fixture(capture_with_context("Recent project update"), vec![]);
    fixture.state.pause_capture();

    let token = fixture.token.clone();
    let (status, value) = call(&fixture, Some(&token), request_body()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({"available": false, "reason": "paused"}));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_applies_the_latest_blacklist_without_returning_content() {
    let fixture = fixture(
        capture_with_context("Private project update"),
        vec![CaptureBlacklistEntry {
            kind: "app_name".to_string(),
            pattern: "Slack".to_string(),
        }],
    );

    let token = fixture.token.clone();
    let (status, value) = call(&fixture, Some(&token), request_body()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({"available": false, "reason": "blacklisted"}));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.tree_reads.load(Ordering::SeqCst), 0);
    assert!(!serde_json::to_string(&value)
        .expect("response JSON")
        .contains("Private project update"));
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_preflights_the_exact_process_and_required_window_title() {
    let fixture = fixture(capture_with_context("Recent project update"), vec![]);
    let token = fixture.token.clone();

    let (pid_status, pid_value) = call(
        &fixture,
        Some(&token),
        json!({
            "expected_pid": 7,
            "expected_window_title": "Roadmap — Slack",
            "expected_window_id": 9_001
        }),
    )
    .await;
    let (title_status, title_value) = call(
        &fixture,
        Some(&token),
        json!({
            "expected_pid": 42,
            "expected_window_title": "Other window",
            "expected_window_id": 9_001
        }),
    )
    .await;
    let (window_status, window_value) = call(
        &fixture,
        Some(&token),
        json!({
            "expected_pid": 42,
            "expected_window_title": "Roadmap — Slack",
            "expected_window_id": 9_002
        }),
    )
    .await;

    assert_eq!(pid_status, StatusCode::OK);
    assert_eq!(title_status, StatusCode::OK);
    assert_eq!(window_status, StatusCode::OK);
    assert_eq!(
        pid_value,
        json!({"available": false, "reason": "wrong_target"})
    );
    assert_eq!(title_value, pid_value);
    assert_eq!(window_value, pid_value);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.tree_reads.load(Ordering::SeqCst), 0);
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_rejects_an_empty_recent_context() {
    let mut capture = capture_with_context("Recent project update");
    capture.root.children[0].children.remove(0);
    let fixture = fixture(capture, vec![]);

    let token = fixture.token.clone();
    let (status, value) = call(&fixture, Some(&token), request_body()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value, json!({"available": false, "reason": "empty"}));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_refuses_secure_input_without_returning_content() {
    let mut capture = capture_with_context("Private project update");
    capture.secure_input = true;
    let fixture = fixture(capture, vec![]);

    let token = fixture.token.clone();
    let (status, value) = call(&fixture, Some(&token), request_body()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        value,
        json!({"available": false, "reason": "capture_unavailable"})
    );
    assert!(!serde_json::to_string(&value)
        .expect("response JSON")
        .contains("Private project update"));
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_response_enforces_item_and_byte_bounds() {
    let mut capture = capture_with_context("discarded fixture text");
    capture.root.children[0].children.clear();
    for index in 0..60 {
        capture.root.children[0].children.push(AccessibilityNode {
            role: "AXStaticText".to_string(),
            frame: Some(rect(340, 100 + index, 400, 20)),
            value: Some(format!("message-{index:02} {}", "x".repeat(300))),
            ..AccessibilityNode::default()
        });
    }
    capture.root.children[0].children.push(AccessibilityNode {
        role: "AXTextArea".to_string(),
        frame: Some(rect(300, 700, 650, 50)),
        value: Some(String::new()),
        placeholder: Some("Message #roadmap".to_string()),
        focused: true,
        ..AccessibilityNode::default()
    });
    let fixture = fixture(capture, vec![]);

    let token = fixture.token.clone();
    let (status, value) = call(&fixture, Some(&token), request_body()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["available"], true);
    let text = value["context"]["text"].as_str().expect("context text");
    assert!(text.len() <= 8 * 1_024);
    assert!(text.lines().count() <= 40);
    assert!(text
        .lines()
        .last()
        .is_some_and(|line| line.starts_with("[left] message-59 ")));
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_returns_only_bounded_ephemeral_target_context() {
    let fixture = fixture(capture_with_context("Recent project update"), vec![]);

    let token = fixture.token.clone();
    let (status, value) = call(
        &fixture,
        Some(&token),
        json!({
            "expected_pid": 42,
            "expected_window_title": "Roadmap — Slack",
            "expected_window_id": 9_001
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        value,
        json!({
            "available": true,
            "context": {
                "app": "Slack",
                "window_title": "Roadmap — Slack",
                "domain": "example.com",
                "text": "[left] Recent project update"
            }
        })
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.tree_reads.load(Ordering::SeqCst), 1);
    assert!(fixture
        .storage
        .export_snapshots()
        .expect("snapshots")
        .is_empty());
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_accepts_only_exact_supported_chat_surfaces() {
    let mut slack_entry_area = capture_with_context("Slack entry-area message");
    slack_entry_area.root.focused = true;
    slack_entry_area.root.children[0].focused = true;
    slack_entry_area.root.children[0].children[1].role = "AXGroup".to_string();
    slack_entry_area.root.children[0].children[1].subrole = Some("AXTextEntryArea".to_string());
    let slack_fixture = fixture(slack_entry_area, vec![]);
    let token = slack_fixture.token.clone();
    let (status, value) = call(&slack_fixture, Some(&token), request_body()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["available"], true);
    let _ = fs::remove_dir_all(slack_fixture.directory);

    let whatsapp = whatsapp_capture_with_context("Can we meet tomorrow?");
    let whatsapp_fixture = fixture(whatsapp, vec![]);
    let token = whatsapp_fixture.token.clone();
    let (status, value) = call(&whatsapp_fixture, Some(&token), request_body()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["available"], true);
    assert_eq!(value["context"]["domain"], "web.whatsapp.com");
    assert_eq!(value["context"]["text"], "[left] Can we meet tomorrow?");
    let _ = fs::remove_dir_all(whatsapp_fixture.directory);

    for browser_url in [
        "http://web.whatsapp.com/chat",
        "https://whatsapp.com/chat",
        "https://evil.web.whatsapp.com/chat",
        "https://web.whatsapp.com.evil.test/chat",
    ] {
        let mut capture = capture_with_context("Private unsupported context");
        capture.app_name = "Google Chrome".to_string();
        capture.bundle_id = Some("com.google.Chrome".to_string());
        capture.browser_url = Some(browser_url.to_string());
        let fixture = fixture(capture, vec![]);
        let token = fixture.token.clone();
        let (status, value) = call(&fixture, Some(&token), request_body()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            value,
            json!({"available": false, "reason": "not_chat_composer"})
        );
        assert!(!serde_json::to_string(&value)
            .expect("response JSON")
            .contains("Private unsupported context"));
        assert_eq!(fixture.tree_reads.load(Ordering::SeqCst), 0);
        let _ = fs::remove_dir_all(fixture.directory);
    }
}

#[tokio::test]
async fn visible_context_rejects_unsupported_or_nonempty_focused_fields() {
    let mut cases = Vec::new();

    let mut wrong_bundle = capture_with_context("Private unsupported context");
    wrong_bundle.bundle_id = Some("com.tinyspeck.slackmacgap.fake".to_string());
    cases.push(wrong_bundle);

    let mut unlabeled_slack_textarea = capture_with_context("Private unsupported context");
    unlabeled_slack_textarea.root.children[0].children[1].placeholder = None;
    cases.push(unlabeled_slack_textarea);

    let mut slack_canvas = capture_with_context("Private unsupported context");
    slack_canvas.root.children[0].children[1].placeholder = Some("Canvas".to_string());
    cases.push(slack_canvas);

    let mut slack_canvas_ancestor = capture_with_context("Private unsupported context");
    let composer = slack_canvas_ancestor.root.children[0].children.remove(1);
    slack_canvas_ancestor.root.children[0]
        .children
        .push(AccessibilityNode {
            role: "AXGroup".to_string(),
            title: Some("Canvas document".to_string()),
            frame: Some(rect(300, 650, 650, 100)),
            children: vec![composer],
            ..AccessibilityNode::default()
        });
    cases.push(slack_canvas_ancestor);

    for role in ["AXSearchField", "AXComboBox", "AXTextField"] {
        let mut capture = capture_with_context("Private unsupported context");
        capture.root.children[0].children[1].role = role.to_string();
        cases.push(capture);
    }

    let mut nonempty = capture_with_context("Private unsupported context");
    nonempty.root.children[0].children[1].value = Some("draft".to_string());
    cases.push(nonempty);

    let mut secure_subrole = capture_with_context("Private unsupported context");
    secure_subrole.root.children[0].children[1].subrole = Some("AXSecureTextField".to_string());
    cases.push(secure_subrole);

    let mut ambiguous = capture_with_context("Private unsupported context");
    ambiguous.root.children[0].children.push(AccessibilityNode {
        role: "AXTextArea".to_string(),
        frame: Some(rect(300, 650, 650, 40)),
        value: Some(String::new()),
        focused: true,
        ..AccessibilityNode::default()
    });
    cases.push(ambiguous);

    let mut whatsapp_without_web_area = capture_with_context("Private unsupported context");
    whatsapp_without_web_area.app_name = "Google Chrome".to_string();
    whatsapp_without_web_area.bundle_id = Some("com.google.Chrome".to_string());
    whatsapp_without_web_area.browser_url = Some("https://web.whatsapp.com/chat".to_string());
    whatsapp_without_web_area.root.children[0].children[1].placeholder =
        Some("Type a message".to_string());
    cases.push(whatsapp_without_web_area);

    let mut whatsapp_wrong_web_area = whatsapp_capture_with_context("Private unsupported context");
    whatsapp_wrong_web_area.root.children[0].url = Some("https://example.com/chat".to_string());
    cases.push(whatsapp_wrong_web_area);

    let mut whatsapp_search = whatsapp_capture_with_context("Private unsupported context");
    whatsapp_search.root.children[0].children[0].children[1].role = "AXSearchField".to_string();
    cases.push(whatsapp_search);

    for capture in cases {
        let fixture = fixture(capture, vec![]);
        let token = fixture.token.clone();
        let (status, value) = call(&fixture, Some(&token), request_body()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            value,
            json!({"available": false, "reason": "not_chat_composer"})
        );
        assert!(!serde_json::to_string(&value)
            .expect("response JSON")
            .contains("Private unsupported context"));
        let _ = fs::remove_dir_all(fixture.directory);
    }
}

#[tokio::test]
async fn visible_context_redacts_the_ephemeral_response_defensively() {
    let private_email = "private@example.com";
    let mut capture = capture_with_context(&format!("Contact {private_email}"));
    capture.window_title = Some(format!("Message from {private_email}"));
    let fixture = fixture(capture, vec![]);

    let token = fixture.token.clone();
    let (status, value) = call(
        &fixture,
        Some(&token),
        json!({
            "expected_pid": 42,
            "expected_window_title": format!("Message from {private_email}"),
            "expected_window_id": 9_001
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["available"], true);
    assert_eq!(
        value["context"]["window_title"],
        "Message from [REDACTED_EMAIL]"
    );
    assert_eq!(value["context"]["text"], "[left] Contact [REDACTED_EMAIL]");
    assert!(!serde_json::to_string(&value)
        .expect("response JSON")
        .contains(private_email));
    assert!(fixture
        .storage
        .export_snapshots()
        .expect("snapshots")
        .is_empty());
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_never_returns_a_truncated_redaction_marker() {
    let text = format!("{} a@b.co", "x".repeat(8_170));
    let fixture = fixture(capture_with_context(&text), vec![]);

    let token = fixture.token.clone();
    let (status, value) = call(&fixture, Some(&token), request_body()).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["available"], true);
    let returned = value["context"]["text"].as_str().expect("context text");
    assert!(returned.len() <= 8 * 1_024);
    assert!(!returned.contains("a@b.co"));
    assert!(!returned.contains("[REDACTED_"));
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_rejects_unbounded_noncanonical_and_unknown_inputs() {
    let fixture = fixture(capture_with_context("Recent project update"), vec![]);
    let token = fixture.token.clone();

    for body in [
        json!({
            "expected_pid": 0,
            "expected_window_title": "Roadmap — Slack",
            "expected_window_id": 9_001
        }),
        json!({
            "expected_pid": 42,
            "expected_window_title": "",
            "expected_window_id": 9_001
        }),
        json!({
            "expected_pid": 42,
            "expected_window_title": " title ",
            "expected_window_id": 9_001
        }),
        json!({
            "expected_pid": 42,
            "expected_window_title": "x".repeat(4 * 1_024 + 1),
            "expected_window_id": 9_001
        }),
        json!({
            "expected_pid": 42,
            "expected_window_title": "Roadmap — Slack",
            "expected_window_id": 0
        }),
    ] {
        let (status, _) = call(&fixture, Some(&token), body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (missing_title_status, _) = call(&fixture, Some(&token), json!({"expected_pid": 42})).await;
    assert_eq!(missing_title_status, StatusCode::UNPROCESSABLE_ENTITY);
    let (overflow_status, _) = call(
        &fixture,
        Some(&token),
        json!({
            "expected_pid": i64::from(i32::MAX) + 1,
            "expected_window_title": "Roadmap — Slack",
            "expected_window_id": 9_001
        }),
    )
    .await;
    assert_eq!(overflow_status, StatusCode::UNPROCESSABLE_ENTITY);
    let (unknown_status, _) = call(
        &fixture,
        Some(&token),
        json!({
            "expected_pid": 42,
            "expected_window_title": "Roadmap — Slack",
            "expected_window_id": 9_001,
            "unexpected": true
        }),
    )
    .await;
    assert_eq!(unknown_status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);

    let (missing_window_status, missing_window_value) = call(
        &fixture,
        Some(&token),
        json!({
            "expected_pid": 42,
            "expected_window_title": "Roadmap — Slack"
        }),
    )
    .await;
    assert_eq!(missing_window_status, StatusCode::OK);
    assert_eq!(missing_window_value["available"], true);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.tree_reads.load(Ordering::SeqCst), 1);
    let _ = fs::remove_dir_all(fixture.directory);
}

#[tokio::test]
async fn visible_context_collapses_provider_failures_to_a_bounded_reason() {
    for outcome in [ProviderOutcome::Blacklisted, ProviderOutcome::Unavailable] {
        let fixture = fixture_with_outcome(outcome, vec![]);
        let token = fixture.token.clone();
        let (status, value) = call(&fixture, Some(&token), request_body()).await;

        assert_eq!(status, StatusCode::OK);
        assert!(matches!(
            value["reason"].as_str(),
            Some("blacklisted" | "capture_unavailable")
        ));
        assert_eq!(value.as_object().expect("response object").len(), 2);
        let _ = fs::remove_dir_all(fixture.directory);
    }
}
