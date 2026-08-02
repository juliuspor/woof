//! Loopback-only HTTP API exposed by the woof daemon.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration as StdDuration,
};

mod memory;
mod semantic;

pub use memory::{
    spawn_memory_service, GeneratedCompletion, GenerationKind, GenerationRequest, MemoryClock,
    MemoryGenerationError, MemoryGenerationGate, MemoryGenerator, MemoryRunReport,
    MemoryScheduleConfig, MemoryScheduler, MemorySupervisor, OpenAiMemoryGenerator,
    SystemMemoryClock, DAY_CHRONICLE_PROMPT, HOUR_CHRONICLE_PROMPT, MONTH_CHRONICLE_PROMPT,
    TIME_RULE_PROMPT, WEEK_CHRONICLE_PROMPT, WIKI_EXTRACTION_PROMPT, WIKI_PAGE_PROMPT,
    YEAR_CHRONICLE_PROMPT,
};
pub use semantic::{
    SemanticInitialization, SemanticSearchService, SemanticServiceError, SharedSemanticSearch,
};

use axum::{
    extract::{Query, Request, State},
    http::{header::AUTHORIZATION, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, RwLock};
use tower_http::{catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer};
use url::Url;
use woof_capture::{
    AccessibilityNode, AccessibilityProvider, BlacklistKind, BlacklistRule, CaptureController,
    CaptureError, CapturePipeline, CapturePolicy, ExponentialBackoff, ForegroundCapture,
    PipelineConfig, PipelineOutcome, RawCapture, Redactor, SkipReason, SnapshotCandidate,
};
use woof_core::{
    atomic_write_private, health_proof, normalize_capture_blacklist, read_private_file_bounded,
    ApiToken, CaptureBlacklistEntry, DataRetentionPolicy, WoofConfig, HEALTH_CHALLENGE_HEADER,
    HEALTH_PROOF_HEADER,
};
use woof_llm::CancellationToken;
use woof_storage::{
    CaptureRecord, ProactiveRule, RetentionPruneReport, Storage, StorageError,
    StorageRecoveryReason,
};
use zeroize::Zeroize;

use crate::semantic::lock_semantic;

#[cfg(target_os = "macos")]
use woof_capture::macos::MacOsAccessibilityProvider;
const DEFAULT_SEARCH_LIMIT: usize = 20;
const MAX_SEARCH_QUERY_BYTES: usize = 1_024;
const MAX_SNAPSHOT_IDS: usize = 100;
const MAX_SNAPSHOT_ID_BYTES: usize = 128;
const MAX_SNAPSHOT_IDS_QUERY_BYTES: usize = 8 * 1_024;
const MAX_CHRONICLE_PERIOD_BYTES: usize = 64;
const MAX_WIKI_SLUG_BYTES: usize = 256;
const MAX_WIKI_SLUG_CHARACTERS: usize = 160;
const MAX_LOCAL_ID_BYTES: usize = 128;
const MAX_IDENTITY_NAME_CHARACTERS: usize = 200;
const MAX_IDENTITY_NAME_BYTES: usize = 800;
const MAX_CHAT_THREAD_ID_BYTES: usize = 128;
const MAX_CHAT_CONTENT_BYTES: usize = 256 * 1_024;
const MAX_INLINE_APP_BYTES: usize = 256;
const MAX_INLINE_DOMAIN_BYTES: usize = 253;
const MAX_INLINE_INSTRUCTION_BYTES: usize = 4 * 1_024;
const MAX_INLINE_OUTPUT_BYTES: usize = 256 * 1_024;
const MAX_VISIBLE_CONTEXT_WINDOW_TITLE_BYTES: usize = 4 * 1_024;
const MAX_VISIBLE_CONTEXT_ITEMS: usize = 40;
const MAX_VISIBLE_CONTEXT_TEXT_BYTES: usize = 8 * 1_024;
const MAX_CLIENT_TIMESTAMP_FUTURE_SECONDS: i64 = 86_400;
const MAX_RULE_LABEL_CHARACTERS: usize = 120;
const MAX_RULE_PROMPT_CHARACTERS: usize = 1_000;
const MAX_RULE_PROMPT_BYTES: usize = 1_000;
const MIN_RULE_INTERVAL_MINUTES: i64 = 5;
const MAX_RULE_INTERVAL_MINUTES: i64 = 7 * 24 * 60;
const MAX_ONE_SHOT_HORIZON_SECONDS: i64 = 10 * 366 * 86_400;
const MAX_PRIVATE_STATE_BYTES: usize = 64 * 1024;

trait AccessibilityAuthorizer: Send + Sync {
    fn is_trusted(&self) -> bool;
    fn request_trust(&self) -> bool;
}

#[derive(Default)]
struct SystemAccessibilityAuthorizer;

impl AccessibilityAuthorizer for SystemAccessibilityAuthorizer {
    fn is_trusted(&self) -> bool {
        daemon_accessibility_trusted()
    }

    fn request_trust(&self) -> bool {
        request_daemon_accessibility()
    }
}

#[cfg(target_os = "macos")]
fn default_visible_context_provider() -> Option<Arc<dyn AccessibilityProvider>> {
    Some(Arc::new(MacOsAccessibilityProvider::default()))
}

#[cfg(not(target_os = "macos"))]
fn default_visible_context_provider() -> Option<Arc<dyn AccessibilityProvider>> {
    None
}

#[derive(Clone, Default)]
pub struct StorageMutationBarrier {
    lock: Arc<AsyncMutex<()>>,
    data_epoch: Arc<AtomicU64>,
}

impl StorageMutationBarrier {
    pub async fn lock(&self) -> OwnedMutexGuard<()> {
        self.lock.clone().lock_owned().await
    }

    pub fn data_epoch(&self) -> u64 {
        self.data_epoch.load(Ordering::SeqCst)
    }

    fn advance_data_epoch(&self) {
        self.data_epoch.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub struct AppState {
    storage: Storage,
    token: Arc<ApiToken>,
    capture_controller: CaptureController,
    capture_runtime: Arc<RwLock<CaptureRuntimeStatus>>,
    capture_policy_gate: Arc<RwLock<()>>,
    blacklist: Arc<RwLock<Vec<CaptureBlacklistEntry>>>,
    retention: Arc<RwLock<DataRetentionPolicy>>,
    identity: Arc<RwLock<Identity>>,
    persisted_config: Option<Arc<RwLock<(PathBuf, WoofConfig)>>>,
    identity_path: Option<PathBuf>,
    nudges_enabled: Arc<RwLock<bool>>,
    semantic: Option<SharedSemanticSearch>,
    database_recovery: Option<StorageRecoveryReason>,
    accessibility_authorizer: Arc<dyn AccessibilityAuthorizer>,
    visible_context_provider: Option<Arc<dyn AccessibilityProvider>>,
    storage_mutation_barrier: StorageMutationBarrier,
    memory_generation_gate: MemoryGenerationGate,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    name: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptureRuntimeStatus {
    pub running: bool,
    pub permission: &'static str,
    pub last_capture_at: Option<i64>,
    pub consecutive_failures: u32,
    pub last_skip: Option<&'static str>,
    pub last_error: Option<&'static str>,
}

impl Default for CaptureRuntimeStatus {
    fn default() -> Self {
        Self {
            running: false,
            permission: "unknown",
            last_capture_at: None,
            consecutive_failures: 0,
            last_skip: None,
            last_error: None,
        }
    }
}

impl AppState {
    pub fn new(storage: Storage, token: ApiToken) -> Self {
        Self {
            storage,
            token: Arc::new(token),
            capture_controller: CaptureController::default(),
            capture_runtime: Arc::new(RwLock::new(CaptureRuntimeStatus::default())),
            capture_policy_gate: Arc::new(RwLock::new(())),
            blacklist: Arc::new(RwLock::new(Vec::new())),
            retention: Arc::new(RwLock::new(DataRetentionPolicy::KeepForever)),
            identity: Arc::new(RwLock::new(Identity::default())),
            persisted_config: None,
            identity_path: None,
            nudges_enabled: Arc::new(RwLock::new(false)),
            semantic: None,
            database_recovery: None,
            accessibility_authorizer: Arc::new(SystemAccessibilityAuthorizer),
            visible_context_provider: default_visible_context_provider(),
            storage_mutation_barrier: StorageMutationBarrier::default(),
            memory_generation_gate: MemoryGenerationGate::default(),
        }
    }

    pub fn with_initial_blacklist(mut self, blacklist: Vec<CaptureBlacklistEntry>) -> Self {
        self.blacklist = Arc::new(RwLock::new(blacklist));
        self
    }

    pub fn storage_mutation_barrier(&self) -> StorageMutationBarrier {
        self.storage_mutation_barrier.clone()
    }

    pub fn memory_generation_gate(&self) -> MemoryGenerationGate {
        self.memory_generation_gate.clone()
    }

    pub fn pause_capture(&self) {
        self.capture_controller.pause();
    }

    pub fn resume_capture(&self) {
        self.capture_controller.resume();
    }

    pub fn with_semantic_search(mut self, service: SemanticSearchService) -> Self {
        self.semantic = Some(service.shared());
        self
    }

    pub fn with_database_recovery(mut self, recovery: Option<StorageRecoveryReason>) -> Self {
        self.database_recovery = recovery;
        self
    }

    pub fn with_visible_context_provider<P>(mut self, provider: P) -> Self
    where
        P: AccessibilityProvider + 'static,
    {
        self.visible_context_provider = Some(Arc::new(provider));
        self
    }

    #[cfg(test)]
    fn with_accessibility_authorizer(
        mut self,
        authorizer: Arc<dyn AccessibilityAuthorizer>,
    ) -> Self {
        self.accessibility_authorizer = authorizer;
        self
    }

    pub fn with_persisted_config(mut self, path: PathBuf, config: WoofConfig) -> Self {
        self.retention = Arc::new(RwLock::new(config.data_retention));
        self.identity_path = Some(config.identity_path.clone());
        if let Some(identity) = self
            .identity_path
            .as_ref()
            .and_then(|identity_path| {
                read_private_file_bounded(identity_path, MAX_PRIVATE_STATE_BYTES).ok()
            })
            .and_then(|bytes| serde_json::from_slice::<Identity>(&bytes).ok())
        {
            self.identity = Arc::new(RwLock::new(identity));
        }
        self.nudges_enabled = Arc::new(RwLock::new(config.nudges_enabled));
        self.persisted_config = Some(Arc::new(RwLock::new((path, config))));
        self
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/search", get(search))
        .route("/snapshots", get(snapshots))
        .route("/recent-activity", get(recent_activity))
        .route("/working-memory", get(working_memory))
        .route("/chronicle", get(chronicle))
        .route("/wiki/list", get(wiki_list))
        .route("/wiki/page", get(wiki_page))
        .route("/wiki/search", get(wiki_search))
        .route("/time/rules", get(time_rules))
        .route("/time/report", get(time_report))
        .route("/capture/accessibility", get(capture_accessibility_status))
        .route(
            "/capture/accessibility/request",
            post(capture_accessibility_request),
        )
        .route("/capture/status", get(capture_status))
        .route("/capture/pause", post(capture_pause))
        .route("/capture/resume", post(capture_resume))
        .route("/capture/blacklist", get(get_blacklist).post(set_blacklist))
        .route("/identity", get(get_identity))
        .route("/identity/set-name", post(set_identity))
        .route("/chronicle/followups", get(chronicle_followups))
        .route("/chronicle/followups/status", post(set_followup_status))
        .route("/nudges/ready-unseen", get(ready_nudges))
        .route("/nudges/item", get(nudge_item))
        .route("/nudges/mark-delivered", post(mark_nudge_delivered))
        .route("/nudges/mark-seen", post(mark_nudge_seen))
        .route("/nudges/dismiss", post(dismiss_nudge))
        .route("/rules", get(list_rules).post(save_rule))
        .route("/rules/delete", post(delete_rule))
        .route("/stats/overview", get(overview_stats))
        .route("/data/retention", get(get_retention).put(set_retention))
        .route("/data/delete-all", post(delete_all_data))
        .route("/chat/record", post(record_chat))
        .route(
            "/preferences/nudges-enabled",
            get(get_nudges_preference).post(set_nudges_preference),
        )
        .route("/work-patterns/status", get(work_pattern_status))
        .route("/work-patterns/update", post(update_work_pattern))
        .route("/inline-rewrite/record", post(record_inline_use))
        .route("/inline-rewrite/record-output", post(record_inline_output))
        .route(
            "/inline-rewrite/visible-context",
            post(visible_inline_context),
        )
        .route(
            "/inline-rewrite/similar-outputs",
            post(similar_inline_outputs_post),
        )
        .fallback(not_found)
        .layer(RequestBodyLimitLayer::new(1024 * 1024))
        .layer(CatchPanicLayer::new())
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

async fn authenticate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if request.method() == Method::GET && request.uri().path() == "/health" {
        return next.run(request).await;
    }
    let authenticated = request
        .headers()
        .get(AUTHORIZATION)
        .map(|value| value.as_bytes())
        .and_then(|value| value.strip_prefix(b"Bearer "))
        .is_some_and(|candidate| state.token.matches_bearer(candidate));
    if authenticated {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Unauthorized"})),
        )
            .into_response()
    }
}

async fn health(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let mut response = Json(json!({"status": "ok"})).into_response();
    let proof = headers
        .get(HEALTH_CHALLENGE_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|challenge| health_proof(&state.token, challenge));
    if let Some(proof) = proof.and_then(|proof| HeaderValue::from_str(&proof).ok()) {
        response.headers_mut().insert(HEALTH_PROOF_HEADER, proof);
    }
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    DEFAULT_SEARCH_LIMIT
}

async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_nonempty_text(
        &query.q,
        MAX_SEARCH_QUERY_BYTES,
        false,
        "q is invalid or too long",
    )?;
    validate_limit(query.limit, 30)?;
    let results = if let Some(semantic) = state.semantic {
        let storage = state.storage;
        tokio::task::spawn_blocking(move || {
            lock_semantic(&semantic)?.search(&storage, &query.q, query.limit)
        })
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(|_| ApiError::internal())?
    } else {
        run_db(state.storage, move |storage| {
            storage.search_snapshots(&query.q, query.limit)
        })
        .await?
    };
    Ok(Json(json!({"results": results})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotsQuery {
    ids: String,
}

async fn snapshots(
    State(state): State<AppState>,
    Query(query): Query<SnapshotsQuery>,
) -> Result<Json<Value>, ApiError> {
    if query.ids.is_empty() || query.ids.len() > MAX_SNAPSHOT_IDS_QUERY_BYTES {
        return Err(ApiError::bad_request(
            "snapshot IDs are invalid or too large",
        ));
    }
    let ids = query
        .ids
        .split(',')
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if ids.len() > MAX_SNAPSHOT_IDS
        || ids
            .iter()
            .any(|id| !valid_local_id(id, MAX_SNAPSHOT_ID_BYTES))
    {
        return Err(ApiError::bad_request(
            "snapshot IDs are invalid or too large",
        ));
    }
    let snapshots = run_db(state.storage, move |storage| storage.snapshots(&ids)).await?;
    Ok(Json(json!({"snapshots": snapshots})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecentQuery {
    #[serde(default = "default_minutes")]
    minutes: u32,
    #[serde(default = "default_recent_limit")]
    limit: usize,
}

fn default_minutes() -> u32 {
    30
}

fn default_recent_limit() -> usize {
    12
}

async fn recent_activity(
    State(state): State<AppState>,
    Query(query): Query<RecentQuery>,
) -> Result<Json<Value>, ApiError> {
    if !(1..=360).contains(&query.minutes) {
        return Err(ApiError::bad_request("minutes is out of range"));
    }
    validate_limit(query.limit, 20)?;
    let activity = run_db(state.storage, move |storage| {
        storage.recent_activity(query.minutes, query.limit)
    })
    .await?;
    Ok(Json(json!({"activity": activity})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LimitQuery {
    #[serde(default = "default_working_memory_limit")]
    limit: usize,
}

fn default_working_memory_limit() -> usize {
    40
}

async fn working_memory(
    State(state): State<AppState>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_limit(query.limit, 200)?;
    let items = run_db(state.storage, move |storage| {
        storage.working_memory(query.limit)
    })
    .await?;
    Ok(Json(json!({"items": items})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChronicleQuery {
    level: String,
    period: String,
}

async fn chronicle(
    State(state): State<AppState>,
    Query(query): Query<ChronicleQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_chronicle_period(&query.level, &query.period)?;
    let chronicle = run_db(state.storage, move |storage| {
        storage.chronicle(&query.level, &query.period)
    })
    .await?;
    Ok(Json(json!({"chronicle": chronicle})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WikiListQuery {
    #[serde(rename = "type")]
    page_type: Option<String>,
    #[serde(default = "default_wiki_limit")]
    limit: usize,
}

fn default_wiki_limit() -> usize {
    50
}

async fn wiki_list(
    State(state): State<AppState>,
    Query(query): Query<WikiListQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_wiki_type(query.page_type.as_deref())?;
    validate_limit(query.limit, 200)?;
    let pages = run_db(state.storage, move |storage| {
        storage.list_wiki(query.page_type.as_deref(), query.limit)
    })
    .await?;
    Ok(Json(json!({"pages": pages})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WikiPageQuery {
    slug: String,
}

async fn wiki_page(
    State(state): State<AppState>,
    Query(query): Query<WikiPageQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_wiki_slug(&query.slug)?;
    let page = run_db(state.storage, move |storage| storage.wiki_page(&query.slug)).await?;
    match page {
        Some(page) => Ok(Json(json!({"page": page}))),
        None => Err(ApiError::not_found("Wiki page not found")),
    }
}

async fn wiki_search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_nonempty_text(
        &query.q,
        MAX_SEARCH_QUERY_BYTES,
        false,
        "q is invalid or too long",
    )?;
    validate_limit(query.limit, 100)?;
    let pages = run_db(state.storage, move |storage| {
        storage.search_wiki(&query.q, query.limit)
    })
    .await?;
    Ok(Json(json!({"pages": pages})))
}

async fn time_rules(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rules = run_db(state.storage, |storage| storage.time_rules()).await?;
    Ok(Json(json!({"rules": rules})))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeReportQuery {
    period: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

async fn time_report(
    State(state): State<AppState>,
    Query(query): Query<TimeReportQuery>,
) -> Result<Json<Value>, ApiError> {
    let (from, to) = resolve_time_range(&query)?;
    let rows = run_db(state.storage, move |storage| storage.time_report(from, to)).await?;
    let total_seconds: f64 = rows.iter().map(|row| row.seconds).sum();
    Ok(Json(json!({
        "from": from,
        "to": to,
        "total_seconds": total_seconds,
        "projects": rows
    })))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowupQuery {
    status: Option<String>,
    #[serde(default = "default_followup_limit")]
    limit: usize,
}

fn default_followup_limit() -> usize {
    50
}

async fn chronicle_followups(
    State(state): State<AppState>,
    Query(query): Query<FollowupQuery>,
) -> Result<Json<Value>, ApiError> {
    if query
        .status
        .as_deref()
        .is_some_and(|status| !["open", "resolved", "dismissed"].contains(&status))
    {
        return Err(ApiError::bad_request("invalid followup status"));
    }
    validate_limit(query.limit, 200)?;
    let followups = run_db(state.storage, move |storage| {
        storage.followups(query.status.as_deref(), query.limit)
    })
    .await?;
    Ok(Json(json!({"followups": followups})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FollowupStatusRequest {
    flag_id: i64,
    status: String,
}

async fn set_followup_status(
    State(state): State<AppState>,
    Json(request): Json<FollowupStatusRequest>,
) -> Result<Json<Value>, ApiError> {
    if request.flag_id <= 0 || !matches!(request.status.as_str(), "resolved" | "dismissed") {
        return Err(ApiError::bad_request("invalid followup status update"));
    }
    let flag_id = request.flag_id;
    let status = request.status;
    let changed_at = chrono::Utc::now().timestamp();
    let updated = run_db_mutation(state, move |storage| {
        storage.set_followup_status(flag_id, &status, changed_at)
    })
    .await?;
    Ok(Json(json!({"updated": updated})))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NudgeListQuery {
    #[serde(default = "default_nudge_limit")]
    limit: usize,
}

fn default_nudge_limit() -> usize {
    20
}

async fn ready_nudges(
    State(state): State<AppState>,
    Query(query): Query<NudgeListQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_limit(query.limit, 50)?;
    if !*state.nudges_enabled.read().await {
        return Ok(Json(json!({"nudges": []})));
    }
    let now = chrono::Utc::now().timestamp();
    let nudges =
        run_db_mutation(state, move |storage| storage.ready_nudges(now, query.limit)).await?;
    Ok(Json(json!({"nudges": nudges})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NudgeItemQuery {
    nudge_id: String,
}

async fn nudge_item(
    State(state): State<AppState>,
    Query(query): Query<NudgeItemQuery>,
) -> Result<Json<Value>, ApiError> {
    if !valid_nudge_id(&query.nudge_id) {
        return Err(ApiError::bad_request("invalid nudge id"));
    }
    let nudge_id = query.nudge_id;
    let nudge = run_db(state.storage, move |storage| storage.ready_nudge(&nudge_id)).await?;
    let Some(nudge) = nudge else {
        return Err(ApiError::not_found("Nudge not found"));
    };
    Ok(Json(json!({"nudge": nudge})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NudgeIdRequest {
    nudge_id: String,
}

async fn mark_nudge_delivered(
    State(state): State<AppState>,
    Json(request): Json<NudgeIdRequest>,
) -> Result<Json<Value>, ApiError> {
    if !valid_nudge_id(&request.nudge_id) {
        return Err(ApiError::bad_request("invalid nudge id"));
    }
    let nudge_id = request.nudge_id;
    let now = chrono::Utc::now().timestamp();
    let delivered = run_db_mutation(state, move |storage| {
        storage.mark_nudge_delivered(&nudge_id, now)
    })
    .await?;
    if !delivered {
        return Err(ApiError::not_found("Nudge not found"));
    }
    Ok(Json(json!({"delivered": true})))
}

async fn mark_nudge_seen(
    State(state): State<AppState>,
    Json(request): Json<NudgeIdRequest>,
) -> Result<Json<Value>, ApiError> {
    let nudge_id = request.nudge_id;
    if !valid_nudge_id(&nudge_id) {
        return Err(ApiError::bad_request("invalid nudge id"));
    }
    let now = chrono::Utc::now().timestamp();
    let seen = run_db_mutation(state, move |storage| {
        storage.mark_nudge_seen(&nudge_id, now)
    })
    .await?;
    if !seen {
        return Err(ApiError::not_found("Nudge not found"));
    }
    Ok(Json(json!({"seen": true})))
}

async fn dismiss_nudge(
    State(state): State<AppState>,
    Json(request): Json<NudgeIdRequest>,
) -> Result<Json<Value>, ApiError> {
    if !valid_nudge_id(&request.nudge_id) {
        return Err(ApiError::bad_request("invalid nudge id"));
    }
    let nudge_id = request.nudge_id;
    let now = chrono::Utc::now().timestamp();
    let dismissed =
        run_db_mutation(state, move |storage| storage.dismiss_nudge(&nudge_id, now)).await?;
    if !dismissed {
        return Err(ApiError::not_found("Nudge not found"));
    }
    Ok(Json(json!({"dismissed": true})))
}

async fn list_rules(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rules = run_db(state.storage, |storage| storage.proactive_rules()).await?;
    Ok(Json(json!({
        "rules": rules.into_iter().map(proactive_rule_value).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProactiveRuleRequest {
    rule_id: Option<String>,
    label: String,
    prompt: String,
    schedule_kind: String,
    #[serde(default)]
    days_of_week: Vec<u8>,
    hour: Option<i64>,
    minute: Option<i64>,
    interval_minutes: Option<i64>,
    timezone: String,
    enabled: Option<bool>,
    fire_at: Option<i64>,
}

async fn save_rule(
    State(state): State<AppState>,
    Json(request): Json<ProactiveRuleRequest>,
) -> Result<Json<Value>, ApiError> {
    let label = request.label.trim();
    let prompt = request.prompt.trim();
    if label.is_empty() || prompt.is_empty() {
        return Err(ApiError::bad_request("rule label and prompt are required"));
    }
    if label.chars().count() > MAX_RULE_LABEL_CHARACTERS
        || prompt.chars().count() > MAX_RULE_PROMPT_CHARACTERS
        || prompt.len() > MAX_RULE_PROMPT_BYTES
        || label.chars().any(char::is_control)
        || prompt.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request("rule text is invalid or too long"));
    }
    let rule_id = request.rule_id;
    if rule_id
        .as_deref()
        .is_some_and(|rule_id| !valid_local_id(rule_id, MAX_LOCAL_ID_BYTES))
    {
        return Err(ApiError::bad_request("invalid rule id"));
    }
    let schedule_kind = request.schedule_kind;
    if !["once", "daily", "weekly", "interval"].contains(&schedule_kind.as_str()) {
        return Err(ApiError::bad_request("invalid schedule kind"));
    }
    let timezone = request.timezone;
    if timezone != "local" {
        return Err(ApiError::bad_request(
            "only local reminder scheduling is supported",
        ));
    }
    let now = chrono::Utc::now().timestamp();
    if request
        .days_of_week
        .iter()
        .any(|day| !(1..=7).contains(day))
        || request
            .days_of_week
            .windows(2)
            .any(|days| days[0] >= days[1])
    {
        return Err(ApiError::bad_request(
            "days_of_week must be sorted unique ISO weekday numbers 1 through 7",
        ));
    }
    let (hour, minute, interval_minutes, fire_at) = match schedule_kind.as_str() {
        "daily" => {
            if !request.days_of_week.is_empty()
                || request.interval_minutes.is_some()
                || request.fire_at.is_some()
            {
                return Err(ApiError::bad_request("invalid daily schedule fields"));
            }
            let (Some(hour), Some(minute)) = (request.hour, request.minute) else {
                return Err(ApiError::bad_request(
                    "daily schedules require hour and minute",
                ));
            };
            validate_rule_clock(hour, minute)?;
            (hour, minute, 0, None)
        }
        "weekly" => {
            if request.days_of_week.is_empty()
                || request.interval_minutes.is_some()
                || request.fire_at.is_some()
            {
                return Err(ApiError::bad_request("invalid weekly schedule fields"));
            }
            let (Some(hour), Some(minute)) = (request.hour, request.minute) else {
                return Err(ApiError::bad_request(
                    "weekly schedules require hour and minute",
                ));
            };
            validate_rule_clock(hour, minute)?;
            (hour, minute, 0, None)
        }
        "interval" => {
            if request.hour.is_some()
                || request.minute.is_some()
                || !request.days_of_week.is_empty()
                || request.fire_at.is_some()
            {
                return Err(ApiError::bad_request("invalid interval schedule fields"));
            }
            let Some(interval) = request.interval_minutes else {
                return Err(ApiError::bad_request(
                    "interval schedules require interval_minutes",
                ));
            };
            if !(MIN_RULE_INTERVAL_MINUTES..=MAX_RULE_INTERVAL_MINUTES).contains(&interval) {
                return Err(ApiError::bad_request("interval_minutes is out of range"));
            }
            (0, 0, interval, None)
        }
        "once" => {
            if request.hour.is_some()
                || request.minute.is_some()
                || !request.days_of_week.is_empty()
                || request.interval_minutes.is_some()
            {
                return Err(ApiError::bad_request("invalid one-shot schedule fields"));
            }
            let Some(fire_at) = request.fire_at else {
                return Err(ApiError::bad_request("one-shot schedules require fire_at"));
            };
            if fire_at <= now || fire_at > now.saturating_add(MAX_ONE_SHOT_HORIZON_SECONDS) {
                return Err(ApiError::bad_request("fire_at is out of range"));
            }
            (0, 0, 0, Some(fire_at))
        }
        _ => unreachable!("schedule kind was validated"),
    };
    let days_of_week = request
        .days_of_week
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let redactor = Redactor::default();
    let rule = ProactiveRule {
        rule_id,
        label: redactor.redact(label).text,
        prompt: redactor.redact(prompt).text,
        schedule_kind,
        days_of_week,
        hour,
        minute,
        interval_minutes,
        timezone,
        enabled: request.enabled.unwrap_or(true),
        created_at: now,
        updated_at: now,
        last_fired_at: None,
        fire_at,
    };
    let rule = run_db_mutation(state, move |storage| storage.save_proactive_rule(rule)).await?;
    Ok(Json(json!({"rule": proactive_rule_value(rule)})))
}

fn validate_rule_clock(hour: i64, minute: i64) -> Result<(), ApiError> {
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) {
        return Err(ApiError::bad_request("invalid rule time"));
    }
    Ok(())
}

fn proactive_rule_value(rule: ProactiveRule) -> Value {
    let days_of_week = if rule.days_of_week.is_empty() {
        Vec::new()
    } else {
        rule.days_of_week
            .split(',')
            .filter_map(|day| day.parse::<u8>().ok())
            .collect()
    };
    json!({
        "rule_id": rule.rule_id,
        "label": rule.label,
        "prompt": rule.prompt,
        "schedule_kind": rule.schedule_kind,
        "days_of_week": days_of_week,
        "hour": rule.hour,
        "minute": rule.minute,
        "interval_minutes": rule.interval_minutes,
        "timezone": rule.timezone,
        "enabled": rule.enabled,
        "created_at": rule.created_at,
        "updated_at": rule.updated_at,
        "last_fired_at": rule.last_fired_at,
        "fire_at": rule.fire_at,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleIdRequest {
    rule_id: String,
}

async fn delete_rule(
    State(state): State<AppState>,
    Json(request): Json<RuleIdRequest>,
) -> Result<Json<Value>, ApiError> {
    let rule_id = request.rule_id;
    if !valid_local_id(&rule_id, MAX_LOCAL_ID_BYTES) {
        return Err(ApiError::bad_request("invalid rule id"));
    }
    let deleted = run_db_mutation(state, move |storage| {
        storage.delete_proactive_rule(&rule_id)
    })
    .await?;
    Ok(Json(json!({"deleted": deleted})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatsQuery {
    #[serde(default = "default_stats_minutes")]
    minutes: u32,
}

fn default_stats_minutes() -> u32 {
    24 * 60
}

fn stats_since(minutes: u32) -> i64 {
    chrono::Utc::now().timestamp() - (i64::from(minutes.clamp(1, 43_200)) * 60)
}

async fn overview_stats(
    State(state): State<AppState>,
    Query(query): Query<StatsQuery>,
) -> Result<Json<Value>, ApiError> {
    if !(1..=43_200).contains(&query.minutes) {
        return Err(ApiError::bad_request("minutes is out of range"));
    }
    let since = stats_since(query.minutes);
    let overview = run_db(state.storage, move |storage| storage.overview_stats(since)).await?;
    Ok(Json(json!({"overview": overview})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatRecordRequest {
    thread_id: Option<String>,
    role: String,
    content: String,
    created_at: Option<i64>,
}

async fn record_chat(
    State(state): State<AppState>,
    Json(request): Json<ChatRecordRequest>,
) -> Result<Json<Value>, ApiError> {
    if !["user", "assistant"].contains(&request.role.as_str()) {
        return Err(ApiError::bad_request("invalid chat role"));
    }
    if request
        .thread_id
        .as_deref()
        .is_some_and(|thread_id| !valid_local_id(thread_id, MAX_CHAT_THREAD_ID_BYTES))
    {
        return Err(ApiError::bad_request("invalid chat thread id"));
    }
    validate_nonempty_text(
        &request.content,
        MAX_CHAT_CONTENT_BYTES,
        true,
        "chat content is invalid or too long",
    )?;
    let now = chrono::Utc::now().timestamp();
    let created_at = validate_optional_client_timestamp(request.created_at, now)?.unwrap_or(now);
    let content = Redactor::default().redact(&request.content).text;
    let turn_id = run_db_mutation(state, move |storage| {
        storage.record_chat_turn(
            request.thread_id.as_deref(),
            &request.role,
            &content,
            created_at,
        )
    })
    .await?;
    Ok(Json(json!({"turn_id": turn_id})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BooleanPreferenceRequest {
    enabled: bool,
}

async fn get_nudges_preference(State(state): State<AppState>) -> Json<Value> {
    Json(json!({"enabled": *state.nudges_enabled.read().await}))
}

async fn set_nudges_preference(
    State(state): State<AppState>,
    Json(request): Json<BooleanPreferenceRequest>,
) -> Result<Json<Value>, ApiError> {
    let enabled = request.enabled;
    let persisted = match state.persisted_config.clone() {
        Some(persisted) => Some(persisted.write_owned().await),
        None => None,
    };
    let mutation_guard = state.storage_mutation_barrier.lock().await;
    let nudges_enabled = state.nudges_enabled.clone();
    tokio::task::spawn_blocking(move || {
        let _mutation_guard = mutation_guard;
        if let Some(mut persisted) = persisted {
            let path = persisted.0.clone();
            let mut config = persisted.1.clone();
            config.nudges_enabled = enabled;
            config.save(&path).map_err(|_| ApiError::internal())?;
            persisted.1 = config;
        }
        *nudges_enabled.blocking_write() = enabled;
        Ok::<_, ApiError>(())
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(json!({"enabled": enabled})))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkPatternQuery {
    #[serde(default = "default_work_pattern_limit")]
    limit: usize,
}

fn default_work_pattern_limit() -> usize {
    20
}

async fn work_pattern_status(
    State(state): State<AppState>,
    Query(query): Query<WorkPatternQuery>,
) -> Result<Json<Value>, ApiError> {
    validate_limit(query.limit, 100)?;
    let status = run_db(state.storage, move |storage| {
        storage.work_pattern_status(query.limit)
    })
    .await?;
    Ok(Json(json!({"status": status})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkPatternUpdateRequest {
    workflow_id: String,
    status: String,
}

async fn update_work_pattern(
    State(state): State<AppState>,
    Json(request): Json<WorkPatternUpdateRequest>,
) -> Result<Json<Value>, ApiError> {
    let workflow_id = request.workflow_id.trim();
    if uuid::Uuid::parse_str(workflow_id)
        .ok()
        .is_none_or(|parsed| parsed.hyphenated().to_string() != workflow_id)
        || !matches!(request.status.as_str(), "accepted" | "dismissed")
    {
        return Err(ApiError::bad_request("invalid work pattern update"));
    }
    let workflow_id = workflow_id.to_string();
    let status = request.status;
    let changed_at = chrono::Utc::now().timestamp();
    let updated = run_db_mutation(state, move |storage| {
        storage.set_workflow_status(&workflow_id, &status, changed_at)
    })
    .await?;
    Ok(Json(json!({"updated": updated})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineUseRequest {
    app: String,
    #[serde(default)]
    domain: String,
    used_at: Option<i64>,
}

async fn record_inline_use(
    State(state): State<AppState>,
    Json(request): Json<InlineUseRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_inline_context(&request.app, &request.domain)?;
    let now = chrono::Utc::now().timestamp();
    let used_at = validate_optional_client_timestamp(request.used_at, now)?.unwrap_or(now);
    let use_count = run_db_mutation(state, move |storage| {
        storage.record_inline_use(&request.app, &request.domain, used_at)
    })
    .await?;
    Ok(Json(json!({"use_count": use_count})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InlineOutputRequest {
    app: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    instruction: String,
    output: String,
    created_at: Option<i64>,
}

async fn record_inline_output(
    State(state): State<AppState>,
    Json(request): Json<InlineOutputRequest>,
) -> Result<Json<Value>, ApiError> {
    validate_inline_context(&request.app, &request.domain)?;
    validate_inline_instruction(&request.instruction)?;
    validate_nonempty_text(
        &request.output,
        MAX_INLINE_OUTPUT_BYTES,
        true,
        "inline output is invalid or too long",
    )?;
    let now = chrono::Utc::now().timestamp();
    let created_at = validate_optional_client_timestamp(request.created_at, now)?.unwrap_or(now);
    let redactor = Redactor::default();
    let instruction = redactor.redact(&request.instruction).text;
    let redacted_output = redactor.redact(&request.output).text;
    let output = run_db_mutation(state, move |storage| {
        storage.record_inline_output(
            &request.app,
            &request.domain,
            &instruction,
            &redacted_output,
            created_at,
        )
    })
    .await?;
    Ok(Json(json!({"output": output})))
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SimilarOutputsRequest {
    #[serde(default)]
    app: String,
    #[serde(default)]
    domain: String,
    #[serde(default)]
    instruction: String,
    #[serde(default = "default_similar_output_limit")]
    limit: usize,
}

fn default_similar_output_limit() -> usize {
    8
}

async fn similar_inline_outputs_post(
    State(state): State<AppState>,
    Json(request): Json<SimilarOutputsRequest>,
) -> Result<Json<Value>, ApiError> {
    similar_inline_outputs(state, request).await
}

async fn similar_inline_outputs(
    state: AppState,
    request: SimilarOutputsRequest,
) -> Result<Json<Value>, ApiError> {
    if !request.app.is_empty() {
        validate_inline_context(&request.app, &request.domain)?;
    } else if !request.domain.is_empty() {
        return Err(ApiError::bad_request("inline app is required with domain"));
    }
    validate_inline_instruction(&request.instruction)?;
    validate_limit(request.limit, 50)?;
    let outputs = run_db(state.storage, move |storage| {
        storage.similar_inline_outputs(
            &request.app,
            &request.domain,
            &request.instruction,
            request.limit,
        )
    })
    .await?;
    Ok(Json(json!({"outputs": outputs})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VisibleContextRequest {
    expected_pid: i32,
    expected_window_title: String,
    expected_window_id: Option<i64>,
}

impl Drop for VisibleContextRequest {
    fn drop(&mut self) {
        self.expected_window_title.zeroize();
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum VisibleContextUnavailableReason {
    Paused,
    Blacklisted,
    WrongTarget,
    NotChatComposer,
    Empty,
    CaptureUnavailable,
}

fn visible_context_unavailable(reason: VisibleContextUnavailableReason) -> Json<Value> {
    Json(json!({"available": false, "reason": reason}))
}

fn exact_https_host(url: Option<&str>, expected_host: &str) -> bool {
    url.and_then(|value| Url::parse(value).ok())
        .is_some_and(|url| {
            url.scheme() == "https"
                && url.host_str() == Some(expected_host)
                && url.port_or_known_default() == Some(443)
        })
}

#[derive(Clone, Copy)]
enum ContextualReplySurface {
    Slack,
    WhatsAppWeb,
}

fn contextual_reply_surface(capture: &RawCapture) -> Option<ContextualReplySurface> {
    if capture.bundle_id.as_deref() == Some("com.tinyspeck.slackmacgap") {
        Some(ContextualReplySurface::Slack)
    } else if exact_https_host(capture.browser_url.as_deref(), "web.whatsapp.com") {
        Some(ContextualReplySurface::WhatsAppWeb)
    } else {
        None
    }
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

fn supported_composer_identity(node: &AccessibilityNode, surface: ContextualReplySurface) -> bool {
    let semantics = [
        node.placeholder.as_deref(),
        node.title.as_deref(),
        node.description.as_deref(),
        node.identifier.as_deref(),
    ];
    semantics.into_iter().flatten().any(|value| {
        let value = value.trim();
        match surface {
            ContextualReplySurface::WhatsAppWeb => [
                "Type a message",
                "Nachricht eingeben",
                "Escribe un mensaje",
                "Écrivez un message",
                "Scrivi un messaggio",
            ]
            .into_iter()
            .any(|supported| value.eq_ignore_ascii_case(supported)),
            ContextualReplySurface::Slack => {
                value.eq_ignore_ascii_case("Message")
                    || starts_with_ignore_ascii_case(value, "Message ")
                    || starts_with_ignore_ascii_case(value, "Nachricht an ")
                    || ["message_input", "message-input", "msg_input", "msg-input"]
                        .into_iter()
                        .any(|supported| value.eq_ignore_ascii_case(supported))
            }
        }
    })
}

fn identifies_slack_canvas(node: &AccessibilityNode) -> bool {
    [
        node.title.as_deref(),
        node.description.as_deref(),
        node.identifier.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| {
        let value = value.trim();
        value.eq_ignore_ascii_case("Canvas")
            || starts_with_ignore_ascii_case(value, "Canvas ")
            || value.eq_ignore_ascii_case("slack_canvas")
            || value.eq_ignore_ascii_case("slack-canvas")
    })
}

fn has_one_empty_message_composer(
    root: &AccessibilityNode,
    surface: ContextualReplySurface,
) -> bool {
    fn visit(
        node: &AccessibilityNode,
        surface: ContextualReplySurface,
        protected_ancestor: bool,
        whatsapp_web_area_ancestor: bool,
        slack_canvas_ancestor: bool,
        focused: &mut Vec<bool>,
    ) {
        let role = node.role.to_ascii_lowercase();
        let subrole = node
            .subrole
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let combined_role = format!("{role} {subrole}");
        let protected = protected_ancestor
            || node.protected
            || combined_role.contains("securetextfield")
            || combined_role.contains("password")
            || combined_role.contains("secure text");
        let whatsapp_web_area_ancestor = whatsapp_web_area_ancestor
            || (role == "axwebarea" && exact_https_host(node.url.as_deref(), "web.whatsapp.com"));
        let slack_canvas_ancestor = slack_canvas_ancestor || identifies_slack_canvas(node);
        let editable_role = matches!(
            role.as_str(),
            "axtextarea" | "axtextfield" | "axsearchfield" | "axcombobox"
        ) || subrole == "axtextentryarea";
        if node.focused && editable_role {
            let message_role = role == "axtextarea" || subrole == "axtextentryarea";
            let supported_ancestry = match surface {
                ContextualReplySurface::Slack => !slack_canvas_ancestor,
                ContextualReplySurface::WhatsAppWeb => whatsapp_web_area_ancestor,
            };
            focused.push(
                !protected
                    && message_role
                    && supported_ancestry
                    && supported_composer_identity(node, surface)
                    && node
                        .value
                        .as_deref()
                        .is_some_and(|value| value.trim().is_empty()),
            );
        }
        if protected {
            return;
        }
        for child in &node.children {
            visit(
                child,
                surface,
                protected,
                whatsapp_web_area_ancestor,
                slack_canvas_ancestor,
                focused,
            );
        }
    }

    let mut focused = Vec::new();
    visit(root, surface, false, false, false, &mut focused);
    focused == [true]
}

fn supports_contextual_reply(capture: &RawCapture) -> bool {
    contextual_reply_surface(capture)
        .is_some_and(|surface| has_one_empty_message_composer(&capture.root, surface))
}

async fn visible_inline_context(
    State(state): State<AppState>,
    Json(request): Json<VisibleContextRequest>,
) -> Result<Json<Value>, ApiError> {
    if request.expected_pid <= 0 {
        return Err(ApiError::bad_request(
            "expected foreground process is invalid",
        ));
    }
    if request
        .expected_window_id
        .is_some_and(|window_id| window_id <= 0)
    {
        return Err(ApiError::bad_request(
            "expected foreground window is invalid",
        ));
    }
    validate_nonempty_text(
        &request.expected_window_title,
        MAX_VISIBLE_CONTEXT_WINDOW_TITLE_BYTES,
        false,
        "expected window title is invalid or too long",
    )?;
    if request.expected_window_title.trim() != request.expected_window_title {
        return Err(ApiError::bad_request(
            "expected window title is invalid or too long",
        ));
    }

    // A shared policy lease makes blacklist and pause updates quiescence
    // boundaries without treating this read-only, ephemeral capture as a
    // policy mutation. The pause check must happen after acquiring the lease.
    let _capture_policy_lease = state.capture_policy_gate.clone().read_owned().await;
    if state.capture_controller.is_paused() {
        return Ok(visible_context_unavailable(
            VisibleContextUnavailableReason::Paused,
        ));
    }

    let entries = state.blacklist.read().await.clone();
    let policy = capture_policy(&entries);
    let Some(provider) = state.visible_context_provider.clone() else {
        return Ok(visible_context_unavailable(
            VisibleContextUnavailableReason::CaptureUnavailable,
        ));
    };
    let captured = match provider
        .capture_foreground_for_target(
            &policy,
            request.expected_pid,
            &request.expected_window_title,
            request.expected_window_id,
        )
        .await
    {
        Ok(ForegroundCapture::Captured(captured)) => captured,
        Ok(ForegroundCapture::Blacklisted) => {
            return Ok(visible_context_unavailable(
                VisibleContextUnavailableReason::Blacklisted,
            ));
        }
        Err(CaptureError::TargetMismatch) => {
            return Ok(visible_context_unavailable(
                VisibleContextUnavailableReason::WrongTarget,
            ));
        }
        Err(CaptureError::UnsupportedSurface) => {
            return Ok(visible_context_unavailable(
                VisibleContextUnavailableReason::NotChatComposer,
            ));
        }
        Err(_) => {
            return Ok(visible_context_unavailable(
                VisibleContextUnavailableReason::CaptureUnavailable,
            ));
        }
    };

    // Keep the policy boundary defensive even for injected providers: a
    // provider may return a capture directly instead of applying preflight.
    if policy.is_blacklisted(&captured) {
        return Ok(visible_context_unavailable(
            VisibleContextUnavailableReason::Blacklisted,
        ));
    }
    if captured.secure_input {
        return Ok(visible_context_unavailable(
            VisibleContextUnavailableReason::CaptureUnavailable,
        ));
    }
    if captured.pid != request.expected_pid
        || captured.window_title.as_deref() != Some(request.expected_window_title.as_str())
        || request
            .expected_window_id
            .is_some_and(|window_id| captured.window_id != Some(window_id))
    {
        return Ok(visible_context_unavailable(
            VisibleContextUnavailableReason::WrongTarget,
        ));
    }
    if !supports_contextual_reply(&captured) {
        return Ok(visible_context_unavailable(
            VisibleContextUnavailableReason::NotChatComposer,
        ));
    }

    let Some(mut unredacted_text) = captured
        .root
        .recent_visible_context_bounded(MAX_VISIBLE_CONTEXT_ITEMS, MAX_VISIBLE_CONTEXT_TEXT_BYTES)
    else {
        return Ok(visible_context_unavailable(
            VisibleContextUnavailableReason::Empty,
        ));
    };
    let redactor = Redactor::default();
    let text =
        redact_bounded_visible_context(&redactor, &unredacted_text, MAX_VISIBLE_CONTEXT_TEXT_BYTES);
    unredacted_text.zeroize();
    if text.trim().is_empty() {
        return Ok(visible_context_unavailable(
            VisibleContextUnavailableReason::Empty,
        ));
    }
    let app = redact_bounded_visible_context(&redactor, &captured.app_name, MAX_INLINE_APP_BYTES);
    let window_title = captured.window_title.as_deref().map(|value| {
        redact_bounded_visible_context(&redactor, value, MAX_VISIBLE_CONTEXT_WINDOW_TITLE_BYTES)
    });
    let domain = captured
        .browser_url
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .map(|value| redact_bounded_visible_context(&redactor, &value, MAX_INLINE_DOMAIN_BYTES));

    Ok(Json(json!({
        "available": true,
        "context": {
            "app": app,
            "window_title": window_title,
            "domain": domain,
            "text": text
        }
    })))
}

async fn capture_status(State(state): State<AppState>) -> Json<Value> {
    let paused = state.capture_controller.is_paused();
    let runtime = state.capture_runtime.read().await.clone();
    let accessibility =
        capture_accessibility_value(state.accessibility_authorizer.is_trusted(), &runtime);
    let accessibility_ready = accessibility.get("ready").and_then(Value::as_bool) == Some(true);
    Json(json!({
        "paused": paused,
        "capturing": !paused && accessibility_ready && capture_runtime_available(&runtime),
        "runtime": runtime,
        "accessibility": accessibility,
        "database_recovery": state.database_recovery.map(|reason| json!({
            "occurred": true,
            "reason": reason.diagnostic_code()
        }))
    }))
}

async fn capture_accessibility_status(State(state): State<AppState>) -> Json<Value> {
    let trusted = state.accessibility_authorizer.is_trusted();
    let runtime = state.capture_runtime.read().await;
    Json(capture_accessibility_value(trusted, &runtime))
}

async fn capture_accessibility_request(State(state): State<AppState>) -> Json<Value> {
    // AXIsProcessTrustedWithOptions is idempotent for an already-trusted
    // process. Calling it here ensures macOS attributes the prompt to woof_d,
    // which is the process that actually performs Accessibility capture.
    let trusted = if state.accessibility_authorizer.is_trusted() {
        true
    } else {
        state.accessibility_authorizer.request_trust()
    };
    let runtime = state.capture_runtime.read().await;
    Json(capture_accessibility_value(trusted, &runtime))
}

fn capture_accessibility_value(trusted: bool, runtime: &CaptureRuntimeStatus) -> Value {
    let operational = trusted && runtime.running;
    json!({
        "trusted": trusted,
        "operational": operational,
        "ready": operational
    })
}

#[cfg(target_os = "macos")]
fn daemon_accessibility_trusted() -> bool {
    MacOsAccessibilityProvider::process_is_trusted()
}

#[cfg(not(target_os = "macos"))]
fn daemon_accessibility_trusted() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn request_daemon_accessibility() -> bool {
    MacOsAccessibilityProvider::request_process_trust()
}

#[cfg(not(target_os = "macos"))]
fn request_daemon_accessibility() -> bool {
    false
}

async fn capture_pause(State(state): State<AppState>) -> Json<Value> {
    // A successful pause response is a quiescence boundary: wait for any AX
    // read and its resulting persistence to finish before pausing.
    let _capture_policy_lease = state.capture_policy_gate.clone().write_owned().await;
    state.capture_controller.pause();
    let runtime = state.capture_runtime.read().await;
    Json(json!({"paused": true, "capturing": false, "runtime": &*runtime}))
}

async fn capture_resume(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let runtime = state.capture_runtime.read().await;
    let accessibility =
        capture_accessibility_value(state.accessibility_authorizer.is_trusted(), &runtime);
    if accessibility.get("ready").and_then(Value::as_bool) != Some(true) {
        return Err(ApiError::conflict(
            "Accessibility is not ready in the local capture service",
        ));
    }
    state.capture_controller.resume();
    Ok(Json(json!({
        "paused": false,
        "capturing": capture_runtime_available(&runtime),
        "runtime": &*runtime,
        "accessibility": accessibility
    })))
}

fn capture_runtime_available(runtime: &CaptureRuntimeStatus) -> bool {
    runtime.running
        && runtime.permission == "granted"
        && !matches!(
            runtime.last_error,
            Some("permission_denied" | "accessibility" | "storage")
        )
}

async fn get_retention(State(state): State<AppState>) -> Json<Value> {
    let retention = *state.retention.read().await;
    Json(json!({"retention": retention}))
}

async fn set_retention(
    State(state): State<AppState>,
    Json(retention): Json<DataRetentionPolicy>,
) -> Result<Json<Value>, ApiError> {
    retention
        .validate()
        .map_err(|_| ApiError::bad_request("data retention must be between 1 and 3650 days"))?;

    let persisted = match state.persisted_config.clone() {
        Some(persisted) => Some(persisted.write_owned().await),
        None => None,
    };
    let mutation_barrier = state.storage_mutation_barrier.clone();
    let mutation_guard = mutation_barrier.lock().await;
    let retention_state = state.retention.clone();
    let storage = state.storage.clone();
    let semantic = state.semantic.clone();
    let now = chrono::Utc::now().timestamp();
    let (pruned, indexed) = tokio::task::spawn_blocking(move || {
        let _mutation_guard = mutation_guard;
        if let Some(mut persisted) = persisted {
            let path = persisted.0.clone();
            let mut config = persisted.1.clone();
            config.data_retention = retention;
            config.save(&path).map_err(|_| ApiError::internal())?;
            persisted.1 = config;
        }
        *retention_state.blocking_write() = retention;
        match retention.cutoff(now) {
            Some(cutoff) => enforce_retention_locked(storage, semantic, &mutation_barrier, cutoff),
            None => Ok((RetentionPruneReport::default(), 0)),
        }
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(json!({
        "retention": retention,
        "pruned": pruned,
        "vector_index": {"indexed": indexed}
    })))
}

async fn delete_all_data(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    // Lock order is part of the reset contract: first quiesce Accessibility
    // capture and its persistence, then cancel/join pre-reset memory prompts,
    // and only then exclude every remaining storage mutation.
    let capture_policy_lease = state.capture_policy_gate.clone().write_owned().await;
    let memory_reset_lease = state.memory_generation_gate.begin_reset().await;
    let mutation_guard = state.storage_mutation_barrier.lock().await;
    let was_paused = state.capture_controller.is_paused();
    state.capture_controller.pause();

    let storage = state.storage.clone();
    let semantic = state.semantic.clone();
    let mutation_barrier = state.storage_mutation_barrier.clone();
    let capture_controller = state.capture_controller.clone();
    let identity = state.identity.clone();
    let identity_path = state.identity_path.clone();
    let joined = tokio::task::spawn_blocking(move || {
        // The owned guard must live in the blocking closure. Dropping or
        // aborting the HTTP future cannot cancel spawn_blocking once it has
        // started, so keeping the guard in the async caller would allow a new
        // daemon to overlap the still-running durable mutation.
        let _capture_policy_lease = capture_policy_lease;
        let _memory_reset_lease = memory_reset_lease;
        let _mutation_guard = mutation_guard;
        let _capture_resume = CaptureResumeOnDrop {
            controller: capture_controller,
            should_resume: !was_paused,
        };
        let deleted_rows = storage.delete_all_data()?;
        let indexed = if let Some(semantic) = semantic {
            lock_semantic(&semantic)
                .and_then(|mut service| service.rebuild(&storage))
                .map(|report| report.indexed)
        } else {
            Ok(0)
        };
        // SQLite deletion has committed. Invalidate every concurrent
        // generation and clear identity even if a derived-index or identity
        // file write fails afterward. These effects stay inside the guarded
        // closure so client cancellation cannot skip them.
        mutation_barrier.advance_data_epoch();
        *identity.blocking_write() = Identity::default();
        let identity_saved = identity_path
            .map(|path| atomic_write_private(&path, b"{}\n").is_ok())
            .unwrap_or(true);
        Ok::<_, StorageError>((deleted_rows, indexed, identity_saved))
    })
    .await;

    let operation = match joined {
        Err(_) => Err(ApiError::internal()),
        Ok(Err(error)) => Err(ApiError::from(error)),
        Ok(Ok((deleted_rows, indexed, identity_saved))) => match (indexed, identity_saved) {
            (Ok(indexed), true) => Ok((deleted_rows, indexed)),
            (Err(_), _) | (_, false) => Err(ApiError::internal()),
        },
    };

    let (deleted_rows, indexed) = operation?;

    Ok(Json(json!({
        "status": "deleted",
        "deleted_rows": deleted_rows,
        "vector_index": {"indexed": indexed}
    })))
}

async fn get_blacklist(State(state): State<AppState>) -> Json<Value> {
    let blacklist = state.blacklist.read().await.clone();
    Json(json!({"blacklist": blacklist}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlacklistRequest {
    blacklist: Vec<CaptureBlacklistEntry>,
}

async fn set_blacklist(
    State(state): State<AppState>,
    Json(request): Json<BlacklistRequest>,
) -> Result<Json<Value>, ApiError> {
    if request.blacklist.iter().any(|entry| {
        entry.kind != entry.kind.trim().to_ascii_lowercase()
            || entry.pattern != entry.pattern.trim()
            || contains_disallowed_controls(&entry.pattern, false)
    }) {
        return Err(ApiError::bad_request(
            "capture blacklist entries must use canonical text",
        ));
    }
    let entries = request.blacklist;
    let entries = normalize_capture_blacklist(entries)
        .map_err(|error| ApiError::bad_request(error.user_message()))?;
    // Queue the exclusive lease before acquiring any persistence lock. Tokio's
    // fair RwLock then prevents a new capture from starting with the old list,
    // while allowing the current read lease to finish cleanly.
    let capture_policy_lease = state.capture_policy_gate.clone().write_owned().await;
    let persisted = match state.persisted_config.clone() {
        Some(persisted) => Some(persisted.write_owned().await),
        None => None,
    };
    let mutation_guard = state.storage_mutation_barrier.lock().await;
    let blacklist = state.blacklist.clone();
    let saved_entries = entries.clone();
    tokio::task::spawn_blocking(move || {
        let _capture_policy_lease = capture_policy_lease;
        let _mutation_guard = mutation_guard;
        if let Some(mut persisted) = persisted {
            let path = persisted.0.clone();
            let mut config = persisted.1.clone();
            config.capture_blacklist = saved_entries.clone();
            config.save(&path).map_err(|_| ApiError::internal())?;
            persisted.1 = config;
        }
        *blacklist.blocking_write() = saved_entries;
        Ok::<_, ApiError>(())
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(json!({"blacklist": entries})))
}

async fn get_identity(State(state): State<AppState>) -> Json<Value> {
    let identity = state.identity.read().await.clone();
    Json(json!({"identity": identity}))
}

async fn set_identity(
    State(state): State<AppState>,
    Json(identity): Json<Identity>,
) -> Result<Json<Value>, ApiError> {
    if let Some(name) = identity.name.as_deref() {
        if name.trim() != name
            || name.is_empty()
            || name.len() > MAX_IDENTITY_NAME_BYTES
            || name.chars().count() > MAX_IDENTITY_NAME_CHARACTERS
            || contains_disallowed_controls(name, false)
        {
            return Err(ApiError::bad_request(
                "identity name is invalid or too long",
            ));
        }
    }
    let mut bytes = serde_json::to_vec_pretty(&identity).map_err(|_| ApiError::internal())?;
    bytes.push(b'\n');
    let mutation_guard = state.storage_mutation_barrier.lock().await;
    let identity_path = state.identity_path.clone();
    let identity_state = state.identity.clone();
    let saved_identity = identity.clone();
    tokio::task::spawn_blocking(move || {
        let _mutation_guard = mutation_guard;
        if let Some(path) = identity_path {
            atomic_write_private(&path, &bytes).map_err(|_| ApiError::internal())?;
        }
        *identity_state.blocking_write() = saved_identity;
        Ok::<_, ApiError>(())
    })
    .await
    .map_err(|_| ApiError::internal())??;
    Ok(Json(json!({"identity": identity})))
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({"error": "Not Found"})))
}

async fn run_db<T, F>(storage: Storage, task: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(Storage) -> Result<T, StorageError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || task(storage))
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::from)
}

async fn run_db_mutation<T, F>(state: AppState, task: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(Storage) -> Result<T, StorageError> + Send + 'static,
{
    let mutation_guard = state.storage_mutation_barrier.lock().await;
    let storage = state.storage;
    tokio::task::spawn_blocking(move || {
        let _mutation_guard = mutation_guard;
        task(storage)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(ApiError::from)
}

async fn enforce_retention(
    state: &AppState,
    now: i64,
) -> Result<(RetentionPruneReport, usize), ApiError> {
    let policy = *state.retention.read().await;
    let Some(cutoff) = policy.cutoff(now) else {
        return Ok((RetentionPruneReport::default(), 0));
    };

    let mutation_barrier = state.storage_mutation_barrier.clone();
    let mutation_guard = mutation_barrier.lock().await;
    let storage = state.storage.clone();
    let semantic = state.semantic.clone();
    tokio::task::spawn_blocking(move || {
        let _mutation_guard = mutation_guard;
        enforce_retention_locked(storage, semantic, &mutation_barrier, cutoff)
    })
    .await
    .map_err(|_| ApiError::internal())?
}

fn enforce_retention_locked(
    storage: Storage,
    semantic: Option<SharedSemanticSearch>,
    mutation_barrier: &StorageMutationBarrier,
    cutoff: i64,
) -> Result<(RetentionPruneReport, usize), ApiError> {
    let report = storage.prune_expired_data(cutoff).map_err(ApiError::from)?;
    let semantic_result = if report.deleted_rows > 0 {
        match semantic {
            Some(semantic) => lock_semantic(&semantic)
                .and_then(|mut service| service.rebuild(&storage))
                .map(|report| report.indexed),
            None => Ok(0),
        }
    } else {
        Ok(0)
    };
    if report.deleted_rows > 0 {
        mutation_barrier.advance_data_epoch();
    }
    let indexed = semantic_result.map_err(|_| ApiError::internal())?;
    Ok((report, indexed))
}

pub struct DaemonSupervisor {
    cancellation: CancellationToken,
    capture_task: tokio::task::JoinHandle<()>,
}

pub struct AutomationSupervisor {
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl AutomationSupervisor {
    pub async fn shutdown(self) {
        self.cancellation.cancel();
        let mut task = self.task;
        if tokio::time::timeout(StdDuration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }
}

/// Enforces retention and turns due proactive rules into notification-ready
/// nudges. Storage mutations are serialized with capture and memory writes.
pub fn spawn_automation_service(
    state: AppState,
    poll_interval: StdDuration,
) -> AutomationSupervisor {
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        loop {
            if task_cancellation.is_cancelled() {
                break;
            }
            let now = chrono::Utc::now().timestamp();
            if enforce_retention(&state, now).await.is_err() {
                tracing::warn!("data retention enforcement failed");
            }
            if *state.nudges_enabled.read().await {
                let result = run_db_mutation(state.clone(), move |storage| {
                    storage.materialize_due_rule_nudges(now, 500)?;
                    storage.promote_due_nudges(now)?;
                    Ok(())
                })
                .await;
                if result.is_err() {
                    tracing::warn!("proactive nudge scheduler storage update failed");
                }
            }
            if sleep_or_cancel(poll_interval, &task_cancellation).await {
                break;
            }
        }
    });
    AutomationSupervisor { cancellation, task }
}

impl DaemonSupervisor {
    pub async fn shutdown(self) {
        self.cancellation.cancel();
        let mut capture_task = self.capture_task;
        if tokio::time::timeout(StdDuration::from_secs(5), &mut capture_task)
            .await
            .is_err()
        {
            capture_task.abort();
            let _ = capture_task.await;
        }
    }
}

#[cfg(target_os = "macos")]
pub async fn spawn_capture_service(
    state: AppState,
    capture_interval: StdDuration,
    coalesce_window: StdDuration,
    working_memory_capacity: usize,
) -> DaemonSupervisor {
    spawn_capture_with_provider(
        state,
        MacOsAccessibilityProvider::default(),
        capture_interval,
        coalesce_window,
        working_memory_capacity,
    )
    .await
}

pub async fn spawn_capture_with_provider<P>(
    state: AppState,
    provider: P,
    capture_interval: StdDuration,
    coalesce_window: StdDuration,
    working_memory_capacity: usize,
) -> DaemonSupervisor
where
    P: AccessibilityProvider + 'static,
{
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let initial_policy = capture_policy(&state.blacklist.read().await);
    let coalesce_window_ms = coalesce_window.as_millis().min(i64::MAX as u128) as i64;
    let mut pipeline = CapturePipeline::new(
        PipelineConfig {
            coalesce_window_ms,
            policy: initial_policy,
            ..PipelineConfig::default()
        },
        state.capture_controller.clone(),
    );
    let capture_task = tokio::spawn(async move {
        {
            let mut runtime = state.capture_runtime.write().await;
            runtime.running = true;
            runtime.permission = "unknown";
        }
        let mut backoff = ExponentialBackoff::default();
        let mut applied_blacklist = state.blacklist.read().await.clone();
        let mut active_snapshot_id: Option<String> = None;
        let mut observed_continuity_epoch = state.capture_controller.continuity_epoch();
        let mut observed_data_epoch = state.storage_mutation_barrier.data_epoch();

        loop {
            if task_cancellation.is_cancelled() {
                break;
            }
            clear_for_controller_discontinuity(
                &state.capture_controller,
                &mut observed_continuity_epoch,
                &mut pipeline,
                &mut active_snapshot_id,
            );
            if state.capture_controller.is_paused() {
                clear_capture_continuity(&mut pipeline, &mut active_snapshot_id);
                if sleep_or_cancel(capture_interval, &task_cancellation).await {
                    break;
                }
                continue;
            }

            // Hold a shared lease from policy selection through persistence.
            // Blacklist, pause, and delete-all endpoints take the exclusive
            // lease, making their successful response a quiescence boundary.
            let capture_policy_lease = tokio::select! {
                () = task_cancellation.cancelled() => break,
                lease = state.capture_policy_gate.clone().read_owned() => lease,
            };
            if state.capture_controller.is_paused() {
                drop(capture_policy_lease);
                clear_capture_continuity(&mut pipeline, &mut active_snapshot_id);
                if sleep_or_cancel(capture_interval, &task_cancellation).await {
                    break;
                }
                continue;
            }

            // Delete-all may have completed while this task waited behind its
            // exclusive lease, so refresh continuity only after acquiring the
            // read lease for this attempt.
            let data_epoch = state.storage_mutation_barrier.data_epoch();
            if data_epoch != observed_data_epoch {
                let latest_blacklist = state.blacklist.read().await.clone();
                pipeline = CapturePipeline::new(
                    PipelineConfig {
                        coalesce_window_ms,
                        policy: capture_policy(&latest_blacklist),
                        ..PipelineConfig::default()
                    },
                    state.capture_controller.clone(),
                );
                applied_blacklist = latest_blacklist;
                active_snapshot_id = None;
                observed_data_epoch = data_epoch;
            }

            let latest_blacklist = state.blacklist.read().await.clone();
            let attempt_policy = capture_policy(&latest_blacklist);
            if latest_blacklist != applied_blacklist {
                pipeline.set_policy(attempt_policy.clone());
                applied_blacklist = latest_blacklist;
            }

            let capture_result = provider.capture_foreground(&attempt_policy).await;
            clear_for_controller_discontinuity(
                &state.capture_controller,
                &mut observed_continuity_epoch,
                &mut pipeline,
                &mut active_snapshot_id,
            );
            let delay = match capture_result {
                Ok(ForegroundCapture::Captured(raw)) => {
                    backoff.record_success();
                    {
                        let mut runtime = state.capture_runtime.write().await;
                        runtime.permission = "granted";
                        runtime.consecutive_failures = 0;
                        runtime.last_error = None;
                    }
                    match pipeline.process(*raw) {
                        PipelineOutcome::Stored(candidate) => {
                            let persisted_snapshot_id = persist_candidate(
                                &state,
                                candidate,
                                None,
                                working_memory_capacity,
                                true,
                            )
                            .await;
                            update_capture_continuity_after_persist(
                                &mut pipeline,
                                &mut active_snapshot_id,
                                persisted_snapshot_id,
                            );
                        }
                        PipelineOutcome::Deduplicated(candidate) => {
                            let current_snapshot_id = active_snapshot_id.take();
                            let persisted_snapshot_id = persist_candidate(
                                &state,
                                candidate,
                                current_snapshot_id,
                                working_memory_capacity,
                                false,
                            )
                            .await;
                            update_capture_continuity_after_persist(
                                &mut pipeline,
                                &mut active_snapshot_id,
                                persisted_snapshot_id,
                            );
                        }
                        PipelineOutcome::Coalesced(candidate) => {
                            let current_snapshot_id = active_snapshot_id.take();
                            let persisted_snapshot_id = persist_candidate(
                                &state,
                                candidate,
                                current_snapshot_id,
                                working_memory_capacity,
                                true,
                            )
                            .await;
                            update_capture_continuity_after_persist(
                                &mut pipeline,
                                &mut active_snapshot_id,
                                persisted_snapshot_id,
                            );
                        }
                        PipelineOutcome::Skipped(reason) => {
                            clear_capture_continuity(&mut pipeline, &mut active_snapshot_id);
                            let mut runtime = state.capture_runtime.write().await;
                            runtime.last_skip = Some(skip_code(reason));
                        }
                    }
                    capture_interval
                }
                Ok(ForegroundCapture::Blacklisted) => {
                    backoff.record_success();
                    clear_capture_continuity(&mut pipeline, &mut active_snapshot_id);
                    let mut runtime = state.capture_runtime.write().await;
                    runtime.permission = "granted";
                    runtime.consecutive_failures = 0;
                    runtime.last_error = None;
                    runtime.last_skip = Some("blacklisted");
                    capture_interval
                }
                Err(error) => {
                    clear_capture_continuity(&mut pipeline, &mut active_snapshot_id);
                    let delay = backoff.record_failure();
                    let mut runtime = state.capture_runtime.write().await;
                    runtime.consecutive_failures = backoff.failures();
                    runtime.last_error = Some(capture_error_code(&error));
                    runtime.last_skip = match error {
                        CaptureError::SecureInput => Some("secure_input"),
                        _ => None,
                    };
                    if matches!(error, CaptureError::PermissionDenied) {
                        runtime.permission = "denied";
                    }
                    delay
                }
            };

            drop(capture_policy_lease);
            if sleep_or_cancel(delay, &task_cancellation).await {
                break;
            }
        }
        state.capture_runtime.write().await.running = false;
    });
    DaemonSupervisor {
        cancellation,
        capture_task,
    }
}

fn clear_for_controller_discontinuity(
    controller: &CaptureController,
    observed_epoch: &mut u64,
    pipeline: &mut CapturePipeline,
    active_snapshot_id: &mut Option<String>,
) {
    let epoch = controller.continuity_epoch();
    if epoch != *observed_epoch {
        clear_capture_continuity(pipeline, active_snapshot_id);
        *observed_epoch = epoch;
    }
}

fn clear_capture_continuity(
    pipeline: &mut CapturePipeline,
    active_snapshot_id: &mut Option<String>,
) {
    pipeline.clear_pending();
    *active_snapshot_id = None;
}

fn update_capture_continuity_after_persist(
    pipeline: &mut CapturePipeline,
    active_snapshot_id: &mut Option<String>,
    persisted_snapshot_id: Option<String>,
) {
    if let Some(snapshot_id) = persisted_snapshot_id {
        *active_snapshot_id = Some(snapshot_id);
    } else {
        clear_capture_continuity(pipeline, active_snapshot_id);
    }
}

async fn persist_candidate(
    state: &AppState,
    candidate: SnapshotCandidate,
    snapshot_id: Option<String>,
    working_memory_capacity: usize,
    semantic_refresh: bool,
) -> Option<String> {
    let capture = candidate_record(candidate, snapshot_id);
    let observed_at = capture.last_seen_at;
    match persist_capture(state, capture, working_memory_capacity, semantic_refresh).await {
        Ok(snapshot_id) => {
            let mut runtime = state.capture_runtime.write().await;
            runtime.last_capture_at = Some(observed_at);
            runtime.last_skip = None;
            runtime.last_error = None;
            Some(snapshot_id)
        }
        Err(PersistCaptureError::Semantic { snapshot_id }) => {
            let mut runtime = state.capture_runtime.write().await;
            runtime.last_capture_at = Some(observed_at);
            runtime.last_skip = None;
            runtime.last_error = Some("semantic_index");
            Some(snapshot_id)
        }
        Err(PersistCaptureError::Storage) => {
            state.capture_runtime.write().await.last_error = Some("storage");
            None
        }
    }
}

#[derive(Debug)]
enum PersistCaptureError {
    Storage,
    Semantic { snapshot_id: String },
}

async fn persist_capture(
    state: &AppState,
    capture: CaptureRecord,
    working_memory_capacity: usize,
    semantic_content_changed: bool,
) -> Result<String, PersistCaptureError> {
    let mutation_guard = state.storage_mutation_barrier.lock().await;
    let storage = state.storage.clone();
    let semantic = state.semantic.clone();
    let capture = redact_capture_record(capture);
    tokio::task::spawn_blocking(move || {
        let _mutation_guard = mutation_guard;
        let snapshot_id = storage
            .record_capture(&capture, working_memory_capacity)
            .map_err(|_| PersistCaptureError::Storage)?;
        let Some(semantic) = semantic else {
            return Ok(snapshot_id);
        };
        let semantic_result = lock_semantic(&semantic).and_then(|mut service| {
            if semantic_content_changed {
                service.upsert_persisted_snapshot(&storage, &snapshot_id)
            } else {
                service.refresh_persisted_snapshot_metadata(&storage, &snapshot_id)
            }
        });
        if semantic_result.is_err() {
            return Err(PersistCaptureError::Semantic { snapshot_id });
        }
        Ok(snapshot_id)
    })
    .await
    .map_err(|_| PersistCaptureError::Storage)?
}

struct CaptureResumeOnDrop {
    controller: CaptureController,
    should_resume: bool,
}

impl Drop for CaptureResumeOnDrop {
    fn drop(&mut self) {
        if self.should_resume {
            self.controller.resume();
        }
    }
}

fn redact_capture_record(mut capture: CaptureRecord) -> CaptureRecord {
    let redactor = Redactor::default();
    capture.content = redactor.redact(&capture.content).text;
    capture.app = redactor.redact(&capture.app).text;
    capture.window_title = redactor.redact(&capture.window_title).text;
    capture.url = capture.url.map(|value| redactor.redact(&value).text);
    capture.domain = capture.domain.map(|value| redactor.redact(&value).text);
    capture.focused_name = capture
        .focused_name
        .map(|value| redactor.redact(&value).text);
    capture.focused_role = capture
        .focused_role
        .map(|value| redactor.redact(&value).text);
    capture.focused_path = capture
        .focused_path
        .map(|value| redactor.redact(&value).text);
    capture
}

fn candidate_record(candidate: SnapshotCandidate, snapshot_id: Option<String>) -> CaptureRecord {
    let domain = candidate
        .browser_url
        .as_deref()
        .and_then(|value| Url::parse(value).ok())
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase));
    let focused_name = candidate.focused_breadcrumbs.last().cloned();
    let focused_path = (!candidate.focused_breadcrumbs.is_empty())
        .then(|| serde_json::to_string(&candidate.focused_breadcrumbs).ok())
        .flatten();
    CaptureRecord {
        snapshot_id,
        content: candidate.text,
        app: candidate.app_name,
        window_title: candidate.window_title.unwrap_or_default(),
        url: candidate.browser_url,
        domain,
        captured_at: candidate.started_at_ms.div_euclid(1_000),
        last_seen_at: candidate.last_seen_at_ms.div_euclid(1_000),
        duration_s: candidate.duration_ms.max(0) as f64 / 1_000.0,
        focused_name,
        focused_role: candidate.focused_role,
        focused_path,
    }
}

fn capture_policy(entries: &[CaptureBlacklistEntry]) -> CapturePolicy {
    CapturePolicy::new(entries.iter().filter_map(|entry| {
        let kind = match entry.kind.as_str() {
            "bundle_id" => BlacklistKind::BundleId,
            "bundle_prefix" => BlacklistKind::BundlePrefix,
            "app_name" => BlacklistKind::AppName,
            "window_title" => BlacklistKind::WindowTitle,
            "browser_host" => BlacklistKind::BrowserHost,
            "regex" => BlacklistKind::Regex,
            _ => return None,
        };
        Some(BlacklistRule {
            kind,
            pattern: entry.pattern.clone(),
        })
    }))
}

async fn sleep_or_cancel(duration: StdDuration, cancellation: &CancellationToken) -> bool {
    tokio::select! {
        () = cancellation.cancelled() => true,
        () = tokio::time::sleep(duration) => false,
    }
}

fn skip_code(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::Paused => "paused",
        SkipReason::SecureInput => "secure_input",
        SkipReason::ProtectedContent => "protected_content",
        SkipReason::Blacklisted => "blacklisted",
        SkipReason::Empty => "empty",
    }
}

fn capture_error_code(error: &CaptureError) -> &'static str {
    match error {
        CaptureError::PermissionDenied => "permission_denied",
        CaptureError::SecureInput => "secure_input",
        CaptureError::NoFocusedApplication => "no_focused_application",
        CaptureError::TargetMismatch => "target_mismatch",
        CaptureError::UnsupportedSurface => "unsupported_surface",
        CaptureError::Accessibility(_) => "accessibility",
    }
}

fn validate_limit(value: usize, maximum: usize) -> Result<(), ApiError> {
    if value == 0 || value > maximum {
        return Err(ApiError::bad_request("limit is out of range"));
    }
    Ok(())
}

fn contains_disallowed_controls(value: &str, allow_multiline: bool) -> bool {
    value.chars().any(|character| {
        character.is_control() && !(allow_multiline && matches!(character, '\n' | '\r' | '\t'))
    })
}

fn redact_bounded_visible_context(
    redactor: &Redactor,
    value: &str,
    maximum_bytes: usize,
) -> String {
    let mut redacted = redactor.redact(value).text;
    if redacted.len() > maximum_bytes {
        let mut boundary = maximum_bytes;
        while !redacted.is_char_boundary(boundary) {
            boundary -= 1;
        }
        redacted.truncate(boundary);
        if let Some(marker_start) = redacted.rfind("[REDACTED_") {
            if !redacted[marker_start..].contains(']') {
                redacted.truncate(marker_start);
                redacted.truncate(redacted.trim_end().len());
            }
        }
    }
    redacted
}

fn validate_nonempty_text(
    value: &str,
    maximum_bytes: usize,
    allow_multiline: bool,
    message: &'static str,
) -> Result<(), ApiError> {
    if value.trim().is_empty()
        || value.len() > maximum_bytes
        || contains_disallowed_controls(value, allow_multiline)
    {
        return Err(ApiError::bad_request(message));
    }
    Ok(())
}

fn valid_local_id(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_nudge_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|identifier| {
        !identifier.is_nil() && identifier.hyphenated().to_string() == value
    })
}

fn validate_optional_client_timestamp(
    value: Option<i64>,
    now: i64,
) -> Result<Option<i64>, ApiError> {
    if value.is_some_and(|timestamp| {
        timestamp < 0 || timestamp > now.saturating_add(MAX_CLIENT_TIMESTAMP_FUTURE_SECONDS)
    }) {
        return Err(ApiError::bad_request("timestamp is out of range"));
    }
    Ok(value)
}

fn validate_inline_context(app: &str, domain: &str) -> Result<(), ApiError> {
    validate_nonempty_text(
        app,
        MAX_INLINE_APP_BYTES,
        false,
        "inline app is invalid or too long",
    )?;
    if app.trim() != app
        || domain.len() > MAX_INLINE_DOMAIN_BYTES
        || domain.trim() != domain
        || contains_disallowed_controls(domain, false)
        || domain.chars().any(char::is_whitespace)
        || domain.contains('/')
        || domain.contains("://")
    {
        return Err(ApiError::bad_request(
            "inline domain is invalid or too long",
        ));
    }
    Ok(())
}

fn validate_inline_instruction(value: &str) -> Result<(), ApiError> {
    if value.len() > MAX_INLINE_INSTRUCTION_BYTES || contains_disallowed_controls(value, true) {
        return Err(ApiError::bad_request(
            "inline instruction is invalid or too long",
        ));
    }
    Ok(())
}

fn validate_wiki_slug(value: &str) -> Result<(), ApiError> {
    let mut previous_separator = false;
    if value.is_empty()
        || value.len() > MAX_WIKI_SLUG_BYTES
        || value.chars().count() > MAX_WIKI_SLUG_CHARACTERS
        || value.starts_with('-')
        || value.ends_with('-')
        || value.to_lowercase() != value
        || value.chars().any(|character| {
            let invalid = !(character.is_alphanumeric() || character == '-')
                || (character == '-' && previous_separator);
            previous_separator = character == '-';
            invalid
        })
    {
        return Err(ApiError::bad_request("wiki slug is invalid or too long"));
    }
    Ok(())
}

fn validate_chronicle_period(level: &str, value: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > MAX_CHRONICLE_PERIOD_BYTES
        || contains_disallowed_controls(value, false)
    {
        return Err(ApiError::bad_request("invalid chronicle period"));
    }
    let canonical = match level {
        "hour" => {
            chrono::NaiveDateTime::parse_from_str(&format!("{value}:00:00"), "%Y-%m-%dT%H:%M:%S")
                .map(|date| date.format("%Y-%m-%dT%H").to_string())
        }
        "day" => NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map(|date| date.format("%Y-%m-%d").to_string()),
        "week" => NaiveDate::parse_from_str(&format!("{value}-1"), "%G-W%V-%u")
            .map(|date| date.format("%G-W%V").to_string()),
        "month" => NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d")
            .map(|date| date.format("%Y-%m").to_string()),
        "year" => NaiveDate::parse_from_str(&format!("{value}-01-01"), "%Y-%m-%d")
            .map(|date| date.format("%Y").to_string()),
        _ => return Err(ApiError::bad_request("invalid chronicle level")),
    };
    if canonical.ok().as_deref() != Some(value) {
        return Err(ApiError::bad_request("invalid chronicle period"));
    }
    Ok(())
}

fn validate_wiki_type(page_type: Option<&str>) -> Result<(), ApiError> {
    if page_type.is_some_and(|page_type| {
        !["person", "project", "topic", "tool", "org"].contains(&page_type)
    }) {
        return Err(ApiError::bad_request("invalid wiki page type"));
    }
    Ok(())
}

fn resolve_time_range(query: &TimeReportQuery) -> Result<(i64, i64), ApiError> {
    if let Some(from_value) = query.from.as_deref() {
        if query.period.is_some() {
            return Err(ApiError::bad_request("period cannot be combined with from"));
        }
        let from = parse_date(from_value)?;
        let through = match query.to.as_deref() {
            Some(to) => parse_date(to)?,
            None => Local::now().date_naive(),
        };
        let to = through
            .succ_opt()
            .ok_or_else(|| ApiError::bad_request("invalid date range"))?;
        if from >= to {
            return Err(ApiError::bad_request("from must be before to"));
        }
        return Ok((
            local_midnight(from)?.timestamp(),
            local_midnight(to)?.timestamp(),
        ));
    }
    if query.to.is_some() {
        return Err(ApiError::bad_request("to requires from"));
    }

    let today = Local::now().date_naive();
    let weekday = i64::from(today.weekday().num_days_from_monday());
    let range = match query.period.as_deref().unwrap_or("today") {
        "today" => (today, today + Duration::days(1)),
        "yesterday" => (today - Duration::days(1), today),
        "this_week" => {
            let start = today - Duration::days(weekday);
            (start, start + Duration::days(7))
        }
        "last_week" => {
            let this_week = today - Duration::days(weekday);
            (this_week - Duration::days(7), this_week)
        }
        "this_month" => {
            let start =
                NaiveDate::from_ymd_opt(today.year(), today.month(), 1).expect("valid month");
            let next = if today.month() == 12 {
                NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(today.year(), today.month() + 1, 1)
            }
            .expect("valid next month");
            (start, next)
        }
        "last_7_days" => (today - Duration::days(6), today + Duration::days(1)),
        "last_30_days" => (today - Duration::days(29), today + Duration::days(1)),
        _ => return Err(ApiError::bad_request("invalid period")),
    };
    Ok((
        local_midnight(range.0)?.timestamp(),
        local_midnight(range.1)?.timestamp(),
    ))
}

fn parse_date(value: &str) -> Result<NaiveDate, ApiError> {
    if value.len() != 10 || contains_disallowed_controls(value, false) {
        return Err(ApiError::bad_request("dates must use YYYY-MM-DD"));
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| ApiError::bad_request("dates must use YYYY-MM-DD"))
}

fn local_midnight(date: NaiveDate) -> Result<chrono::DateTime<Local>, ApiError> {
    Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight is valid"))
        .single()
        .ok_or_else(|| ApiError::bad_request("invalid local date"))
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: &'static str,
}

impl ApiError {
    fn bad_request(message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    fn not_found(message: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message,
        }
    }

    fn conflict(message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message,
        }
    }

    fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "Internal server error",
        }
    }
}

impl From<StorageError> for ApiError {
    fn from(error: StorageError) -> Self {
        match error {
            StorageError::InvalidReminder | StorageError::UnsupportedReminderTimezone => {
                Self::bad_request("reminder is invalid or the rule limit was reached")
            }
            _ => Self::internal(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

pub fn public_tool_routes() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("get_chronicle", "/chronicle"),
        ("get_recent_activity", "/recent-activity"),
        ("get_snapshots", "/snapshots"),
        ("get_time_report", "/time/report"),
        ("get_working_memory", "/working-memory"),
        ("get_wiki_page", "/wiki/page"),
        ("list_time_rules", "/time/rules"),
        ("list_wiki", "/wiki/list"),
        ("search_memory", "/search"),
        ("search_wiki", "/wiki/search"),
    ])
}

#[cfg(test)]
mod concurrency_tests {
    use std::{
        fs,
        sync::atomic::{AtomicBool, AtomicUsize},
        time::SystemTime,
    };

    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;
    use woof_storage::TimeRuleWrite;

    static ACCESSIBILITY_TEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct FakeAccessibilityAuthorizer {
        trusted: AtomicBool,
        request_result: bool,
        request_count: AtomicUsize,
    }

    impl FakeAccessibilityAuthorizer {
        fn new(trusted: bool, request_result: bool) -> Self {
            Self {
                trusted: AtomicBool::new(trusted),
                request_result,
                request_count: AtomicUsize::new(0),
            }
        }
    }

    impl AccessibilityAuthorizer for FakeAccessibilityAuthorizer {
        fn is_trusted(&self) -> bool {
            self.trusted.load(Ordering::SeqCst)
        }

        fn request_trust(&self) -> bool {
            self.request_count.fetch_add(1, Ordering::SeqCst);
            self.request_result
        }
    }

    fn accessibility_test_state(
        authorizer: Arc<FakeAccessibilityAuthorizer>,
    ) -> (AppState, String, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let sequence = ACCESSIBILITY_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "woof-accessibility-route-{}-{unique}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("unique temporary directory");
        let storage = Storage::open(directory.join("woof.db")).expect("storage");
        let token = "a".repeat(64);
        let api_token = ApiToken::parse_file(&directory.join("token"), token.as_bytes().to_vec())
            .expect("token");
        (
            AppState::new(storage, api_token).with_accessibility_authorizer(authorizer),
            token,
            directory,
        )
    }

    async fn accessibility_route(
        app: &Router,
        method: Method,
        path: &str,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let status = response.status();
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let value = serde_json::from_slice(&body).expect("JSON response");
        (status, value)
    }

    #[test]
    fn accessibility_readiness_requires_daemon_trust_and_running_capture_service() {
        let mut runtime = CaptureRuntimeStatus::default();
        assert_eq!(capture_accessibility_value(false, &runtime)["ready"], false);
        assert_eq!(capture_accessibility_value(true, &runtime)["ready"], false);

        runtime.running = true;
        assert_eq!(capture_accessibility_value(false, &runtime)["ready"], false);
        assert_eq!(capture_accessibility_value(true, &runtime)["ready"], true);
    }

    #[tokio::test]
    async fn accessibility_prompt_is_authenticated_explicit_and_single_shot() {
        let authorizer = Arc::new(FakeAccessibilityAuthorizer::new(false, false));
        let (state, token, directory) = accessibility_test_state(authorizer.clone());
        let app = router(state);

        let (status, _) =
            accessibility_route(&app, Method::POST, "/capture/accessibility/request", None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(authorizer.request_count.load(Ordering::SeqCst), 0);

        let (status, value) =
            accessibility_route(&app, Method::GET, "/capture/accessibility", Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["ready"], false);
        assert_eq!(authorizer.request_count.load(Ordering::SeqCst), 0);

        let (status, _) = accessibility_route(
            &app,
            Method::POST,
            "/capture/accessibility/request",
            Some(&token),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(authorizer.request_count.load(Ordering::SeqCst), 1);
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn trusted_paused_daemon_is_operational_without_capturing() {
        let authorizer = Arc::new(FakeAccessibilityAuthorizer::new(true, true));
        let (state, token, directory) = accessibility_test_state(authorizer);
        state.pause_capture();
        state.capture_runtime.write().await.running = true;
        let app = router(state);

        let (status, value) =
            accessibility_route(&app, Method::GET, "/capture/accessibility", Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["trusted"], true);
        assert_eq!(value["operational"], true);
        assert_eq!(value["ready"], true);
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn revoked_accessibility_between_check_and_resume_stays_paused() {
        let authorizer = Arc::new(FakeAccessibilityAuthorizer::new(true, true));
        let (state, token, directory) = accessibility_test_state(authorizer.clone());
        state.pause_capture();
        state.capture_runtime.write().await.running = true;
        let app = router(state.clone());

        let (status, value) =
            accessibility_route(&app, Method::GET, "/capture/accessibility", Some(&token)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["ready"], true);

        authorizer.trusted.store(false, Ordering::SeqCst);
        let (status, _) =
            accessibility_route(&app, Method::POST, "/capture/resume", Some(&token)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(state.capture_controller.is_paused());
        let _ = fs::remove_dir_all(directory);
    }

    fn discontinuity_capture(captured_at_ms: i64) -> woof_capture::RawCapture {
        woof_capture::RawCapture {
            captured_at_ms,
            pid: 42,
            app_name: "TextEdit".to_string(),
            bundle_id: Some("com.apple.TextEdit".to_string()),
            window_title: Some("Fixture".to_string()),
            window_id: None,
            browser_url: None,
            secure_input: false,
            root: woof_capture::AccessibilityNode {
                role: "AXTextArea".to_string(),
                value: Some("Synthetic capture".to_string()),
                focused: true,
                ..woof_capture::AccessibilityNode::default()
            },
        }
    }

    #[test]
    fn capture_availability_rejects_permission_and_storage_failures() {
        let mut runtime = CaptureRuntimeStatus {
            running: true,
            permission: "granted",
            ..CaptureRuntimeStatus::default()
        };
        assert!(capture_runtime_available(&runtime));
        runtime.permission = "denied";
        assert!(!capture_runtime_available(&runtime));
        runtime.permission = "granted";
        runtime.last_error = Some("storage");
        assert!(!capture_runtime_available(&runtime));
        runtime.last_error = Some("secure_input");
        assert!(capture_runtime_available(&runtime));
    }

    #[test]
    fn controller_discontinuity_clears_pipeline_and_active_snapshot() {
        let controller = CaptureController::default();
        let mut observed_epoch = controller.continuity_epoch();
        let mut pipeline = CapturePipeline::new(PipelineConfig::default(), controller.clone());
        let outcome = pipeline.process(discontinuity_capture(1_000));
        assert!(matches!(outcome, PipelineOutcome::Stored(_)));
        let mut active_snapshot_id = Some("active-snapshot".to_string());

        controller.pause();
        controller.resume();
        clear_for_controller_discontinuity(
            &controller,
            &mut observed_epoch,
            &mut pipeline,
            &mut active_snapshot_id,
        );

        assert!(pipeline.pending().is_none());
        assert!(active_snapshot_id.is_none());
    }

    #[test]
    fn failed_persistence_clears_duration_continuity_before_recovery() {
        let mut pipeline =
            CapturePipeline::new(PipelineConfig::default(), CaptureController::default());
        assert!(matches!(
            pipeline.process(discontinuity_capture(1_000)),
            PipelineOutcome::Stored(_)
        ));
        let mut active_snapshot_id = Some("active-snapshot".to_string());

        update_capture_continuity_after_persist(&mut pipeline, &mut active_snapshot_id, None);

        assert!(pipeline.pending().is_none());
        assert!(active_snapshot_id.is_none());
        let PipelineOutcome::Stored(candidate) = pipeline.process(discontinuity_capture(20_000))
        else {
            panic!("recovery must begin a fresh snapshot");
        };
        assert_eq!(candidate.started_at_ms, 20_000);
        assert_eq!(candidate.duration_ms, 0);
    }

    #[tokio::test]
    async fn captured_focused_role_survives_the_daemon_storage_path() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("woof-focused-role-{}-{unique}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let storage = Storage::open(directory.join("woof.db")).unwrap();
        let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
        let state = AppState::new(storage.clone(), token);
        let snapshot_id = "focused-role-snapshot".to_string();
        let capture = candidate_record(
            SnapshotCandidate {
                started_at_ms: 1_000,
                last_seen_at_ms: 2_000,
                duration_ms: 1_000,
                pid: 42,
                app_name: "TextEdit".to_string(),
                bundle_id: Some("com.apple.TextEdit".to_string()),
                window_title: Some("Fixture".to_string()),
                browser_url: None,
                focused_breadcrumbs: vec!["Fixture".to_string(), "Editor".to_string()],
                focused_role: Some("AXTextArea".to_string()),
                text: "Synthetic focused-role fixture".to_string(),
                content_hash: [7; 32],
            },
            Some(snapshot_id.clone()),
        );

        assert_eq!(capture.focused_role.as_deref(), Some("AXTextArea"));
        persist_capture(&state, capture, 20, false)
            .await
            .expect("persist capture");
        let snapshots = storage.snapshots(&[snapshot_id]).expect("snapshots");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].focused_role.as_deref(), Some("AXTextArea"));
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn capture_mutation_waits_for_the_storage_reset_barrier() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "woof-mutation-barrier-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let storage = Storage::open(directory.join("woof.db")).unwrap();
        let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
        let state = AppState::new(storage.clone(), token);
        let exclusive = state.storage_mutation_barrier.lock().await;
        let capture_state = state.clone();
        let capture_task = tokio::spawn(async move {
            persist_capture(
                &capture_state,
                CaptureRecord {
                    snapshot_id: Some("barrier-snapshot".to_string()),
                    content: "Synthetic barrier fixture".to_string(),
                    app: "TextEdit".to_string(),
                    window_title: "Fixture".to_string(),
                    url: None,
                    domain: None,
                    captured_at: 1,
                    last_seen_at: 1,
                    duration_s: 1.0,
                    focused_name: None,
                    focused_role: None,
                    focused_path: None,
                },
                20,
                true,
            )
            .await
        });
        tokio::time::sleep(StdDuration::from_millis(25)).await;
        assert!(!capture_task.is_finished());
        assert!(storage
            .snapshots(&["barrier-snapshot".to_string()])
            .unwrap()
            .is_empty());

        drop(exclusive);
        assert!(
            tokio::time::timeout(StdDuration::from_secs(1), capture_task)
                .await
                .expect("capture unblocked")
                .expect("capture task")
                .is_ok()
        );
        assert_eq!(
            storage
                .snapshots(&["barrier-snapshot".to_string()])
                .unwrap()
                .len(),
            1
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn authenticated_database_mutations_share_the_storage_reset_barrier() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "woof-http-mutation-barrier-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let storage = Storage::open(directory.join("woof.db")).unwrap();
        let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
        let state = AppState::new(storage.clone(), token);
        let exclusive = state.storage_mutation_barrier.lock().await;
        let mutation_state = state.clone();
        let mutation_task = tokio::spawn(async move {
            run_db_mutation(mutation_state, |storage| {
                storage.save_time_rule(
                    None,
                    &TimeRuleWrite {
                        project: "Synthetic".to_string(),
                        app: Some("TextEdit".to_string()),
                        domain: None,
                        title_contains: Some("Fixture".to_string()),
                        source: "user".to_string(),
                        created_at: 1,
                    },
                )
            })
            .await
        });
        tokio::time::sleep(StdDuration::from_millis(25)).await;
        assert!(!mutation_task.is_finished());
        assert!(storage.time_rules().unwrap().is_empty());

        drop(exclusive);
        assert!(
            tokio::time::timeout(StdDuration::from_secs(1), mutation_task)
                .await
                .expect("mutation unblocked")
                .expect("mutation task")
                .is_ok()
        );
        assert_eq!(storage.time_rules().unwrap().len(), 1);
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn cancelled_mutation_future_keeps_barrier_until_blocking_work_finishes() {
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "woof-detached-mutation-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let storage = Storage::open(directory.join("woof.db")).unwrap();
        let token = ApiToken::parse_file(&directory.join("token"), vec![b'a'; 64]).expect("token");
        let state = AppState::new(storage, token);
        let barrier = state.storage_mutation_barrier();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mutation_task = tokio::spawn(async move {
            run_db_mutation(state, move |_| {
                started_tx.send(()).expect("signal blocking work");
                release_rx.recv().expect("release blocking work");
                Ok(())
            })
            .await
        });
        tokio::task::spawn_blocking(move || {
            started_rx
                .recv_timeout(StdDuration::from_secs(1))
                .expect("blocking mutation started")
        })
        .await
        .expect("join start observer");

        mutation_task.abort();
        assert!(mutation_task
            .await
            .expect_err("async task cancelled")
            .is_cancelled());
        assert!(
            tokio::time::timeout(StdDuration::from_millis(50), barrier.lock())
                .await
                .is_err(),
            "the blocking closure must retain the barrier after its async waiter is aborted"
        );

        release_tx.send(()).expect("release blocking mutation");
        let guard = tokio::time::timeout(StdDuration::from_secs(1), barrier.lock())
            .await
            .expect("barrier released after blocking mutation");
        drop(guard);
        let _ = fs::remove_dir_all(directory);
    }
}
