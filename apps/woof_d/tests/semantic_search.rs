use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
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
use serde_json::Value;
use tower::ServiceExt;
use woof_core::ApiToken;
use woof_d::{AppState, SemanticSearchService};
use woof_search::{Embedder, SearchError, DIMENSIONS};
use woof_storage::{CaptureRecord, Storage};

static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct SyntheticEmbedder;

impl Embedder for SyntheticEmbedder {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, SearchError> {
        Ok(texts.iter().map(|text| synthetic_vector(text)).collect())
    }
}

fn synthetic_vector(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0; DIMENSIONS];
    let text = text.to_ascii_lowercase();
    if text.contains("dog") || text.contains("canine") {
        vector[0] = 1.0;
    } else {
        vector[1] = 1.0;
    }
    vector
}

struct Fixture {
    app: Router,
    token: String,
    index_path: PathBuf,
    directory: PathBuf,
}

fn fixture(capture: &CaptureRecord) -> Fixture {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "woof-semantic-http-{}-{unique}-{sequence}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("temporary directory");
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    let index_path = directory.join("woof.hnsw.v2");
    let (mut semantic, _) = SemanticSearchService::initialize_with_embedder(
        &storage,
        &index_path,
        Arc::new(SyntheticEmbedder),
    )
    .expect("semantic service");
    let snapshot_id = storage.record_capture(capture, 40).expect("record capture");
    semantic
        .upsert_persisted_snapshot(&storage, &snapshot_id)
        .expect("incremental semantic upsert");
    let token = "a".repeat(64);
    let api_token = ApiToken::parse_file(&directory.join("api-token"), token.as_bytes().to_vec())
        .expect("token");
    let app = woof_d::router(AppState::new(storage, api_token).with_semantic_search(semantic));
    Fixture {
        app,
        token,
        index_path,
        directory,
    }
}

async fn call(app: &Router, token: &str, uri: &str) -> Value {
    let builder = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("response body");
    serde_json::from_slice(&bytes).expect("JSON response")
}

#[tokio::test]
async fn capture_incrementally_updates_hybrid_search_without_changing_response_shape() {
    let capture = CaptureRecord {
        snapshot_id: Some("semantic-dog".to_string()),
        content: "Private canine roadmap fixture".to_string(),
        app: "TextEdit".to_string(),
        window_title: "Synthetic fixture".to_string(),
        url: None,
        domain: None,
        captured_at: 10,
        last_seen_at: 10,
        duration_s: 1.0,
        focused_name: None,
        focused_role: None,
        focused_path: None,
    };
    let fixture = fixture(&capture);

    let result = call(&fixture.app, &fixture.token, "/search?q=dog&limit=20").await;
    assert_eq!(result["results"][0]["snapshot_id"], "semantic-dog");
    let keys = result["results"][0]
        .as_object()
        .expect("search hit")
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        keys,
        [
            "app",
            "captured_at",
            "content_excerpt",
            "domain",
            "score",
            "snapshot_id",
            "window_title",
        ]
        .into_iter()
        .map(ToString::to_string)
        .collect()
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&fixture.index_path)
            .expect("index metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let _ = fs::remove_dir_all(&fixture.directory);
}
