use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use axum::{
    body::Body,
    http::{header::AUTHORIZATION, Request, StatusCode},
};
use tokio::sync::Notify;
use tower::ServiceExt;
use woof_capture::{
    capture_after_preflight, AccessibilityNode, AccessibilityProvider, CaptureError,
    CaptureMetadata, CapturePolicy, ForegroundCapture, RawCapture,
};
use woof_core::{ApiToken, CaptureBlacklistEntry};
use woof_d::SemanticSearchService;
use woof_search::{Embedder, SearchError, DIMENSIONS};
use woof_storage::{Snapshot, Storage};

static DIRECTORY_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

struct CountingEmbedder {
    embedded: Arc<AtomicUsize>,
}

impl Embedder for CountingEmbedder {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, SearchError> {
        self.embedded.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts
            .iter()
            .map(|_| {
                let mut vector = vec![0.0; DIMENSIONS];
                vector[0] = 1.0;
                vector
            })
            .collect())
    }
}

struct SyntheticProvider {
    timestamp_ms: AtomicI64,
}

impl Default for SyntheticProvider {
    fn default() -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_millis()
            .min(i64::MAX as u128) as i64;
        Self {
            timestamp_ms: AtomicI64::new(now_ms),
        }
    }
}

#[async_trait]
impl AccessibilityProvider for SyntheticProvider {
    async fn capture_foreground(
        &self,
        policy: &CapturePolicy,
    ) -> Result<ForegroundCapture, CaptureError> {
        let timestamp_ms = self.timestamp_ms.fetch_add(1_000, Ordering::SeqCst) + 1_000;
        Ok(apply_policy(
            RawCapture {
                captured_at_ms: timestamp_ms,
                pid: 42,
                app_name: "TextEdit".to_string(),
                bundle_id: Some("com.apple.TextEdit".to_string()),
                window_title: Some("Synthetic capture".to_string()),
                window_id: None,
                browser_url: None,
                secure_input: false,
                root: AccessibilityNode {
                    role: "AXWindow".to_string(),
                    title: Some("Synthetic capture".to_string()),
                    children: vec![AccessibilityNode {
                        role: "AXTextArea".to_string(),
                        value: Some("Synthetic memory. Contact private@example.com".to_string()),
                        focused: true,
                        ..AccessibilityNode::default()
                    }],
                    ..AccessibilityNode::default()
                },
            },
            policy,
        ))
    }
}

struct StartupWindowProvider {
    calls: Arc<AtomicUsize>,
    inner: SyntheticProvider,
}

struct ScriptedProvider {
    calls: Arc<AtomicUsize>,
    steps: Mutex<VecDeque<Result<RawCapture, CaptureError>>>,
}

impl ScriptedProvider {
    fn new(
        calls: Arc<AtomicUsize>,
        steps: impl IntoIterator<Item = Result<RawCapture, CaptureError>>,
    ) -> Self {
        Self {
            calls,
            steps: Mutex::new(steps.into_iter().collect()),
        }
    }
}

#[async_trait]
impl AccessibilityProvider for ScriptedProvider {
    async fn capture_foreground(
        &self,
        policy: &CapturePolicy,
    ) -> Result<ForegroundCapture, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let capture = self
            .steps
            .lock()
            .expect("scripted provider lock")
            .pop_front()
            .unwrap_or(Err(CaptureError::NoFocusedApplication))?;
        Ok(apply_policy(capture, policy))
    }
}

fn apply_policy(mut capture: RawCapture, policy: &CapturePolicy) -> ForegroundCapture {
    if policy.is_blacklisted(&capture) {
        capture.zeroize_sensitive();
        ForegroundCapture::Blacklisted
    } else {
        ForegroundCapture::Captured(Box::new(capture))
    }
}

struct MetadataPreflightProvider {
    metadata_reads: Arc<AtomicUsize>,
    full_tree_reads: Arc<AtomicUsize>,
    app_name: &'static str,
    bundle_id: &'static str,
    window_title: Option<&'static str>,
    browser_url: Option<&'static str>,
}

struct InFlightPolicyProvider {
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl AccessibilityProvider for InFlightPolicyProvider {
    async fn capture_foreground(
        &self,
        policy: &CapturePolicy,
    ) -> Result<ForegroundCapture, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let capture = scripted_capture(1_000);
        self.started.notify_one();
        self.release.notified().await;
        Ok(apply_policy(capture, policy))
    }
}

#[async_trait]
impl AccessibilityProvider for MetadataPreflightProvider {
    async fn capture_foreground(
        &self,
        policy: &CapturePolicy,
    ) -> Result<ForegroundCapture, CaptureError> {
        self.metadata_reads.fetch_add(1, Ordering::SeqCst);
        let metadata = CaptureMetadata {
            captured_at_ms: 1_000,
            pid: 42,
            app_name: self.app_name.to_owned(),
            bundle_id: Some(self.bundle_id.to_owned()),
            window_title: self.window_title.map(str::to_owned),
            window_id: None,
            browser_url: self.browser_url.map(str::to_owned),
        };
        let captured = capture_after_preflight(metadata, policy, || {
            self.full_tree_reads.fetch_add(1, Ordering::SeqCst);
            Ok(AccessibilityNode {
                role: "AXWindow".to_owned(),
                children: vec![AccessibilityNode {
                    role: "AXTextArea".to_owned(),
                    value: Some("text that must never be read for an exclusion".to_owned()),
                    focused: true,
                    ..AccessibilityNode::default()
                }],
                ..AccessibilityNode::default()
            })
        })?;
        Ok(match captured {
            Some((metadata, root)) => {
                ForegroundCapture::Captured(Box::new(metadata.into_raw_capture(root)))
            }
            None => ForegroundCapture::Blacklisted,
        })
    }
}

fn scripted_capture(captured_at_ms: i64) -> RawCapture {
    RawCapture {
        captured_at_ms,
        pid: 42,
        app_name: "TextEdit".to_string(),
        bundle_id: Some("com.apple.TextEdit".to_string()),
        window_title: Some("Capture discontinuity fixture".to_string()),
        window_id: None,
        browser_url: None,
        secure_input: false,
        root: AccessibilityNode {
            role: "AXWindow".to_string(),
            children: vec![AccessibilityNode {
                role: "AXTextArea".to_string(),
                value: Some("Synthetic discontinuity memory".to_string()),
                focused: true,
                ..AccessibilityNode::default()
            }],
            ..AccessibilityNode::default()
        },
    }
}

async fn wait_for_snapshots(storage: &Storage, expected: usize) -> Vec<Snapshot> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let exports = storage.export_snapshots().expect("export snapshots");
        if exports.len() >= expected {
            let ids = exports
                .into_iter()
                .map(|snapshot| snapshot.snapshot_id)
                .collect::<Vec<_>>();
            return storage.snapshots(&ids).expect("load snapshots");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {expected} snapshots before deadline"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn assert_two_fresh_snapshots(mut snapshots: Vec<Snapshot>) {
    snapshots.sort_by_key(|snapshot| snapshot.captured_at);
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].captured_at, 1);
    assert_eq!(snapshots[1].captured_at, 20);
    for snapshot in snapshots {
        assert_eq!(snapshot.duration_s, 0.0);
        assert_eq!(snapshot.sighting_count, 1);
    }
}

#[async_trait]
impl AccessibilityProvider for StartupWindowProvider {
    async fn capture_foreground(
        &self,
        policy: &CapturePolicy,
    ) -> Result<ForegroundCapture, CaptureError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inner.capture_foreground(policy).await
    }
}

fn temporary_directory() -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let sequence = DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "woof-supervisor-{}-{unique}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("temporary directory");
    directory
}

#[tokio::test]
async fn supervised_capture_redacts_coalesces_persists_and_stops() {
    let directory = temporary_directory();
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
    let embedded = Arc::new(AtomicUsize::new(0));
    let (semantic, _) = SemanticSearchService::initialize_with_embedder(
        &storage,
        directory.join("woof.vector-index"),
        Arc::new(CountingEmbedder {
            embedded: embedded.clone(),
        }),
    )
    .expect("semantic search");
    let signature_embeddings = embedded.load(Ordering::SeqCst);
    assert_eq!(signature_embeddings, 3);
    let state = woof_d::AppState::new(storage.clone(), token).with_semantic_search(semantic);
    let supervisor = woof_d::spawn_capture_with_provider(
        state,
        SyntheticProvider::default(),
        Duration::from_millis(5),
        Duration::from_secs(30),
        40,
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let hits = loop {
        let hits = storage
            .search_snapshots("Synthetic", 20)
            .expect("search captured data");
        if !hits.is_empty() {
            break hits;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "capture was not persisted before deadline"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    supervisor.shutdown().await;

    let snapshots = storage
        .snapshots(&[hits[0].snapshot_id.clone()])
        .expect("snapshot");
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].content.contains("[REDACTED_EMAIL]"));
    assert!(!snapshots[0].content.contains("private@example.com"));
    assert!(
        snapshots[0].sighting_count >= 2,
        "deduplicated captures should update the same row"
    );
    assert_eq!(
        embedded
            .load(Ordering::SeqCst)
            .saturating_sub(signature_embeddings),
        1,
        "exact deduplications must not re-embed unchanged text"
    );
    assert_eq!(storage.recent_activity(360, 20).expect("activity").len(), 1);
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn capture_started_from_a_paused_state_has_no_startup_window() {
    let directory = temporary_directory();
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
    let state = woof_d::AppState::new(storage.clone(), token);
    state.pause_capture();
    let calls = Arc::new(AtomicUsize::new(0));
    let supervisor = woof_d::spawn_capture_with_provider(
        state,
        StartupWindowProvider {
            calls: calls.clone(),
            inner: SyntheticProvider::default(),
        },
        Duration::from_millis(5),
        Duration::from_secs(30),
        40,
    )
    .await;

    tokio::time::sleep(Duration::from_millis(40)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(storage
        .search_snapshots("Synthetic", 20)
        .expect("search")
        .is_empty());
    supervisor.shutdown().await;
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn exclusions_stop_app_window_and_domain_captures_before_full_tree_read() {
    let cases = [
        (
            "app_name",
            "safari",
            "Safari",
            "com.apple.Safari",
            Some("Quarterly plan"),
            None,
        ),
        (
            "window_title",
            "private payroll",
            "TextEdit",
            "com.apple.TextEdit",
            Some("Private payroll review"),
            None,
        ),
        (
            "browser_host",
            "example.com",
            "Safari",
            "com.apple.Safari",
            Some("Quarterly plan"),
            Some("https://secret.example.com/report"),
        ),
    ];

    for (kind, pattern, app_name, bundle_id, window_title, browser_url) in cases {
        let directory = temporary_directory();
        let storage = Storage::open(directory.join("woof.db")).expect("storage");
        let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
        let metadata_reads = Arc::new(AtomicUsize::new(0));
        let full_tree_reads = Arc::new(AtomicUsize::new(0));
        let state = woof_d::AppState::new(storage.clone(), token).with_initial_blacklist(vec![
            CaptureBlacklistEntry {
                kind: kind.to_owned(),
                pattern: pattern.to_owned(),
            },
        ]);
        let supervisor = woof_d::spawn_capture_with_provider(
            state,
            MetadataPreflightProvider {
                metadata_reads: metadata_reads.clone(),
                full_tree_reads: full_tree_reads.clone(),
                app_name,
                bundle_id,
                window_title,
                browser_url,
            },
            Duration::from_millis(5),
            Duration::from_secs(30),
            40,
        )
        .await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while metadata_reads.load(Ordering::SeqCst) == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{kind} preflight was not attempted"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        supervisor.shutdown().await;

        assert_eq!(
            full_tree_reads.load(Ordering::SeqCst),
            0,
            "{kind} exclusion must reject metadata before reading AX text"
        );
        assert!(
            storage
                .export_snapshots()
                .expect("export snapshots")
                .is_empty(),
            "{kind} exclusion must not persist a capture"
        );
        let _ = fs::remove_dir_all(directory);
    }
}

async fn assert_capture_boundary_waits_for_provider(
    route: &'static str,
    body: &'static str,
    expected_snapshots: usize,
) {
    let directory = temporary_directory();
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    let token_value = "a".repeat(64);
    let token = ApiToken::parse_file(&directory.join("token"), token_value.as_bytes().to_vec())
        .expect("token");
    let state = woof_d::AppState::new(storage.clone(), token);
    let app = woof_d::router(state.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let supervisor = woof_d::spawn_capture_with_provider(
        state,
        InFlightPolicyProvider {
            calls: calls.clone(),
            started: started.clone(),
            release: release.clone(),
        },
        Duration::from_secs(1),
        Duration::from_secs(30),
        40,
    )
    .await;

    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("provider did not enter its capture call");
    let request = Request::builder()
        .method("POST")
        .uri(route)
        .header(AUTHORIZATION, format!("Bearer {token_value}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .expect("request");
    let boundary = tokio::spawn(async move { app.oneshot(request).await.expect("response") });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(
        !boundary.is_finished(),
        "{route} returned while the old provider traversal was still active"
    );

    release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(2), boundary)
        .await
        .expect("boundary request timed out")
        .expect("boundary task");
    assert_eq!(response.status(), StatusCode::OK, "{route} response");
    supervisor.shutdown().await;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "{route} must not allow a second provider call before shutdown"
    );
    assert_eq!(
        storage.export_snapshots().expect("export snapshots").len(),
        expected_snapshots,
        "{route} durable boundary"
    );
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn policy_pause_and_delete_responses_wait_for_active_capture() {
    assert_capture_boundary_waits_for_provider(
        "/capture/blacklist",
        r#"{"blacklist":[{"kind":"app_name","pattern":"textedit"}]}"#,
        1,
    )
    .await;
    assert_capture_boundary_waits_for_provider("/capture/pause", "", 1).await;
    assert_capture_boundary_waits_for_provider("/data/delete-all", "", 0).await;
}

#[tokio::test]
async fn an_observed_pause_starts_a_new_snapshot_without_charging_the_gap() {
    let directory = temporary_directory();
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
    let state = woof_d::AppState::new(storage.clone(), token);
    let capture_control = state.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let supervisor = woof_d::spawn_capture_with_provider(
        state,
        ScriptedProvider::new(
            calls.clone(),
            [Ok(scripted_capture(1_000)), Ok(scripted_capture(20_000))],
        ),
        Duration::from_millis(150),
        Duration::from_secs(30),
        40,
    )
    .await;

    let _ = wait_for_snapshots(&storage, 1).await;
    capture_control.pause_capture();
    tokio::time::sleep(Duration::from_millis(220)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the paused loop must not call the provider"
    );
    capture_control.resume_capture();

    let snapshots = wait_for_snapshots(&storage, 2).await;
    capture_control.pause_capture();
    supervisor.shutdown().await;
    assert_two_fresh_snapshots(snapshots);
    let _ = fs::remove_dir_all(directory);
}

async fn assert_provider_error_breaks_continuity(error: CaptureError) {
    let directory = temporary_directory();
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
    let state = woof_d::AppState::new(storage.clone(), token);
    let capture_control = state.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let supervisor = woof_d::spawn_capture_with_provider(
        state,
        ScriptedProvider::new(
            calls.clone(),
            [
                Ok(scripted_capture(1_000)),
                Err(error),
                Ok(scripted_capture(20_000)),
            ],
        ),
        Duration::from_millis(5),
        Duration::from_secs(30),
        40,
    )
    .await;

    let snapshots = wait_for_snapshots(&storage, 2).await;
    capture_control.pause_capture();
    supervisor.shutdown().await;
    assert!(calls.load(Ordering::SeqCst) >= 3);
    assert_two_fresh_snapshots(snapshots);
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn every_provider_error_starts_a_new_snapshot_without_charging_the_gap() {
    for error in [
        CaptureError::PermissionDenied,
        CaptureError::SecureInput,
        CaptureError::NoFocusedApplication,
        CaptureError::Accessibility("synthetic provider failure".to_string()),
    ] {
        assert_provider_error_breaks_continuity(error).await;
    }
}
