//! Private SQLite persistence for woof.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions, Permissions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

use chrono::{Datelike, Local, TimeZone};
use rusqlite::config::DbConfig;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use woof_core::{ensure_private_dir, read_private_file_bounded, PrivateFileError};

pub const SCHEMA_VERSION: i64 = 18;
pub const SCHEMA_SQL: &str = include_str!("schema.sql");
const MAX_PROACTIVE_RULES: i64 = 500;
const MAX_REMINDER_PROMPT_BYTES: usize = 1_000;
const DATABASE_QUARANTINE_DIRECTORY: &str = "database-quarantine";
const MAX_QUARANTINE_DIRECTORY_ATTEMPTS: usize = 8;
const MAX_QUARANTINE_INCIDENTS: usize = 256;
const MAX_INCIDENT_STATE_BYTES: usize = 8 * 1024;
const INCIDENT_STATE_FILE: &str = "state.json";
const INCIDENT_STATE_TEMP_FILE: &str = ".state.tmp";
const QUARANTINED_DATABASE_FILE: &str = "database.sqlite3";
const INCIDENT_FORMAT_VERSION: u8 = 1;
const SECURE_ERASE_BUFFER_BYTES: usize = 64 * 1024;
const DATABASE_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];
const VIRTUAL_TABLES: [&str; 3] = ["index_fts", "snapshots_fts", "wiki_fts"];
const SCHEMA_TRIGGERS: [&str; 8] = [
    "index_entries_ad",
    "index_entries_ai",
    "index_entries_au",
    "snapshots_ai",
    "snapshots_au",
    "wiki_pages_ad",
    "wiki_pages_ai",
    "wiki_pages_au",
];

pub const LOGICAL_TABLES: [&str; 19] = [
    "snapshots",
    "activity_events",
    "working_memory",
    "chronicle",
    "index_entries",
    "kg_entities",
    "kg_relations",
    "workflows",
    "nudges",
    "proactive_rules",
    "recording_events",
    "inline_rewrite_uses",
    "wiki_pages",
    "salient_flags",
    "chat_turns",
    "inline_outputs",
    "time_ledger",
    "time_rules",
    "style_notes",
];

pub const NAMED_INDEXES: [&str; 33] = [
    "idx_activity_last_seen",
    "idx_activity_snapshot",
    "idx_chat_turns_created",
    "idx_chat_turns_unconsumed",
    "idx_chron_level",
    "idx_chron_level_period",
    "idx_flags_created",
    "idx_flags_dedupe",
    "idx_flags_kind_status",
    "idx_idx_entries_domain",
    "idx_idx_entries_topic_domain",
    "idx_inline_outputs_created",
    "idx_kg_name_type",
    "idx_kg_pred",
    "idx_kg_subj",
    "idx_nudges_dedupe",
    "idx_nudges_scheduled",
    "idx_nudges_status",
    "idx_prules_enabled",
    "idx_recev_session",
    "idx_snap_app",
    "idx_snap_captured",
    "idx_snap_domain",
    "idx_style_notes_source_surface",
    "idx_time_ledger_hour",
    "idx_wf_last_seen",
    "idx_wf_name_lower",
    "idx_wf_status",
    "idx_wiki_dirty",
    "idx_wiki_last_seen",
    "idx_wiki_type",
    "idx_wm_added_at",
    "idx_wm_relevance",
];

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("SQLite operation failed")]
    Sqlite(rusqlite::Error),
    #[error(transparent)]
    PrivateFile(#[from] PrivateFileError),
    #[error("database user_version {found} is unsupported; woof requires version {required}")]
    UnsupportedVersion { found: i64, required: i64 },
    #[error("database integrity check failed")]
    IntegrityCheckFailed,
    #[error("database schema does not match the woof version 18 contract")]
    IncompatibleSchema,
    #[error("database changed while startup recovery was being prepared")]
    DatabaseChangedDuringRecovery,
    #[error("database recovery was interrupted")]
    RecoveryInterrupted,
    #[error("database quarantine has an unexpected structure")]
    UnexpectedQuarantineStructure,
    #[error("reminder timezone must be local")]
    UnsupportedReminderTimezone,
    #[error("reminder schedule is invalid")]
    InvalidReminder,
    #[error("database path is a symlink: {0}")]
    Symlink(PathBuf),
    #[error("database path is not a regular file: {0}")]
    NotRegularFile(PathBuf),
    #[error("database path has more than one hard link: {0}")]
    HardLink(PathBuf),
    #[error("I/O error for {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StorageRecoveryReason {
    Corrupt,
    IncompatibleSchema,
    UnsupportedVersion,
}

impl StorageRecoveryReason {
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Corrupt => "corrupt",
            Self::IncompatibleSchema => "incompatible-schema",
            Self::UnsupportedVersion => "unsupported-version",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageRecovery {
    pub reason: StorageRecoveryReason,
    pub quarantined_database_path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct StorageStartup {
    pub storage: Storage,
    pub recovery: Option<StorageRecovery>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum IncidentPhase {
    Pending,
    Ready,
    Finalized,
    Purging,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct IncidentFile {
    suffix: String,
    source_identity: DatabaseFileIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct IncidentState {
    format_version: u8,
    phase: IncidentPhase,
    created_at: i64,
    reason: StorageRecoveryReason,
    files: Vec<IncidentFile>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryOperation {
    QuarantineRootCreated,
    QuarantineRootParentSynced,
    IncidentDirectoryCreated,
    IncidentDirectorySynced,
    StateTempCreated,
    StateTempWritten,
    StateTempSynced,
    StateRenamed,
    StateDirectorySynced,
    CopyCreated(usize),
    CopyWritten(usize),
    CopySynced(usize),
    CopiesDirectorySynced,
    SourcesRevalidated,
    OriginalRemoved(usize),
    SourceDirectorySynced,
}

fn io_error(path: &Path, source: std::io::Error) -> StorageError {
    StorageError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Clone, Debug)]
pub struct Storage {
    path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Snapshot {
    pub snapshot_id: String,
    pub content: String,
    pub app: String,
    pub window_title: String,
    pub url: Option<String>,
    pub domain: Option<String>,
    pub captured_at: i64,
    pub last_seen_at: i64,
    pub duration_s: f64,
    pub sighting_count: i64,
    pub focused_name: Option<String>,
    pub focused_role: Option<String>,
    pub focused_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchHit {
    pub snapshot_id: String,
    pub app: String,
    pub window_title: String,
    pub domain: Option<String>,
    pub captured_at: i64,
    pub content_excerpt: String,
    pub score: f64,
}

/// Content-bearing snapshot projection used to rebuild the local vector index.
///
/// This stays separate from `SearchHit` so captured text never has to be
/// exposed by the external search route merely to maintain semantic search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotExport {
    pub snapshot_id: String,
    pub content: String,
    pub last_seen_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Activity {
    pub event_id: i64,
    pub snapshot_id: String,
    pub app: String,
    pub window_title: String,
    pub url: Option<String>,
    pub domain: Option<String>,
    pub started_at: i64,
    pub last_seen_at: i64,
    pub duration_s: f64,
    pub content_excerpt: String,
    pub focused_name: Option<String>,
    pub focused_role: Option<String>,
    pub focused_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkingMemoryItem {
    pub wm_id: i64,
    pub added_at: i64,
    pub relevance: f64,
    #[serde(flatten)]
    pub snapshot: Snapshot,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Chronicle {
    pub chronicle_id: Option<String>,
    pub level: String,
    pub period_key: String,
    pub summary_text: String,
    pub snapshot_ids: String,
    pub child_ids: String,
    pub token_count: Option<i64>,
    pub generated_at: i64,
    pub model_used: Option<String>,
    pub is_dirty: i64,
}

#[derive(Clone, Debug)]
pub struct ChronicleWrite {
    pub chronicle_id: String,
    pub level: String,
    pub period_key: String,
    pub summary_text: String,
    pub snapshot_ids: String,
    pub child_ids: String,
    pub token_count: Option<i64>,
    pub generated_at: i64,
    pub model_used: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WikiPage {
    pub slug: Option<String>,
    pub page_type: String,
    pub title: String,
    pub aliases: String,
    pub summary: String,
    pub body: String,
    pub links: String,
    pub snapshot_ids: String,
    pub mention_count: i64,
    pub first_seen: i64,
    pub last_seen: i64,
    pub is_dirty: i64,
    pub updated_at: i64,
    pub model_used: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WikiSummary {
    pub slug: Option<String>,
    pub page_type: String,
    pub title: String,
    pub summary: String,
    pub mention_count: i64,
    pub last_seen: i64,
}

#[derive(Clone, Debug)]
pub struct WikiPageWrite {
    pub slug: String,
    pub page_type: String,
    pub title: String,
    pub aliases: String,
    pub summary: String,
    pub body: String,
    pub links: String,
    pub snapshot_ids: String,
    pub mention_count: i64,
    pub first_seen: i64,
    pub last_seen: i64,
    pub updated_at: i64,
    pub model_used: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TimeRule {
    pub rule_id: i64,
    pub project: String,
    pub app: Option<String>,
    pub domain: Option<String>,
    pub title_contains: Option<String>,
    pub source: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TimeReportRow {
    pub project: String,
    pub seconds: f64,
    pub by_day: BTreeMap<String, f64>,
    pub top_segments: Vec<TimeSegment>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TimeSegment {
    pub app: String,
    pub domain: String,
    pub title: String,
    pub seconds: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnmatchedTimeSegment {
    pub app: String,
    pub domain: String,
    pub window_title: String,
    pub minutes: f64,
}

#[derive(Clone, Debug)]
pub struct TimeRuleWrite {
    pub project: String,
    pub app: Option<String>,
    pub domain: Option<String>,
    pub title_contains: Option<String>,
    pub source: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CaptureRecord {
    pub snapshot_id: Option<String>,
    pub content: String,
    pub app: String,
    pub window_title: String,
    pub url: Option<String>,
    pub domain: Option<String>,
    pub captured_at: i64,
    pub last_seen_at: i64,
    pub duration_s: f64,
    pub focused_name: Option<String>,
    pub focused_role: Option<String>,
    pub focused_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SalientFlag {
    pub flag_id: i64,
    pub kind: String,
    pub text: String,
    pub snapshot_id: Option<String>,
    pub period_key: String,
    pub status: String,
    pub created_at: i64,
}

#[derive(Clone, Debug)]
pub struct SalientFlagWrite {
    pub kind: String,
    pub text: String,
    pub snapshot_id: Option<String>,
    pub period_key: String,
    pub created_at: i64,
}

#[derive(Clone, Debug)]
pub struct HourMemoryWrite {
    pub chronicle: ChronicleWrite,
    pub wiki_pages: Vec<WikiPageWrite>,
    pub flags: Vec<SalientFlagWrite>,
    pub time_rules: Vec<TimeRuleWrite>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Nudge {
    pub nudge_id: Option<String>,
    pub kind: String,
    pub dedupe_key: Option<String>,
    pub scheduled_for: i64,
    pub title: String,
    pub body: String,
    pub deep_link: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub sent_at: Option<i64>,
    pub seen_at: Option<i64>,
    pub dismissed_at: Option<i64>,
    pub meta_json: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProactiveRule {
    pub rule_id: Option<String>,
    pub label: String,
    pub prompt: String,
    pub schedule_kind: String,
    pub days_of_week: String,
    pub hour: i64,
    pub minute: i64,
    pub interval_minutes: i64,
    pub timezone: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_fired_at: Option<i64>,
    pub fire_at: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct OverviewStats {
    pub snapshots: i64,
    pub activity_events: i64,
    pub total_duration_s: f64,
    pub active_apps: i64,
    pub places: i64,
    pub wiki_pages: i64,
    pub open_followups: i64,
    pub workflows: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowSummary {
    pub workflow_id: Option<String>,
    pub name: String,
    pub excerpt: String,
    pub apps: Vec<String>,
    pub frequency_label: String,
    pub observations: Vec<WorkflowObservation>,
    pub status: String,
    pub confidence: f64,
    pub first_detected_at: i64,
    pub last_detected_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkflowObservation {
    pub snapshot_id: String,
    pub app: String,
    pub domain: Option<String>,
    pub window_title: String,
    pub started_at: i64,
    pub last_seen_at: i64,
    pub duration_s: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WorkPatternStatus {
    pub total: i64,
    pub by_status: BTreeMap<String, i64>,
    pub recent: Vec<WorkflowSummary>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InlineOutput {
    pub output_id: i64,
    pub app: String,
    pub domain: String,
    pub instruction: String,
    pub output: String,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct RetentionPruneReport {
    pub deleted_rows: usize,
    pub expired_snapshots: usize,
    pub expired_source_rows: usize,
}

impl Storage {
    /// Opens the database, preserving an unusable database family in a private
    /// quarantine before creating a fresh version 18 database. Only durable
    /// incompatibility or SQLite's explicit corruption classifications trigger
    /// replacement; unsafe paths, locks, permissions, and I/O failures remain
    /// terminal so transient failures cannot displace a valid database.
    pub fn open_or_recover(path: impl AsRef<Path>) -> Result<StorageStartup, StorageError> {
        let mut no_fault = |_| Ok(());
        Self::open_or_recover_with(path.as_ref(), &mut no_fault)
    }

    fn open_or_recover_with(
        path: &Path,
        after_operation: &mut dyn FnMut(RecoveryOperation) -> Result<(), StorageError>,
    ) -> Result<StorageStartup, StorageError> {
        let path = path.to_path_buf();
        let reconciled = reconcile_quarantine_incidents(&path, after_operation)?;
        for attempt in 0..2 {
            let candidate_identity = database_file_identity(&path)?;
            match Self::open(&path) {
                Ok(storage) => {
                    return Ok(StorageStartup {
                        storage,
                        recovery: reconciled,
                    });
                }
                Err(error) => {
                    let Some(reason) = recoverable_startup_reason(&error) else {
                        return Err(error);
                    };
                    let Some(candidate_identity) = candidate_identity else {
                        return Err(error);
                    };
                    match quarantine_database_family(
                        &path,
                        &candidate_identity,
                        reason,
                        after_operation,
                    ) {
                        Ok(quarantined_database_path) => {
                            let storage = Self::open(&path)?;
                            return Ok(StorageStartup {
                                storage,
                                recovery: Some(StorageRecovery {
                                    reason,
                                    quarantined_database_path,
                                }),
                            });
                        }
                        Err(StorageError::DatabaseChangedDuringRecovery) if attempt == 0 => {
                            continue;
                        }
                        Err(recovery_error) => return Err(recovery_error),
                    }
                }
            }
        }
        Err(StorageError::DatabaseChangedDuringRecovery)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        prepare_database_file(&path)?;
        reject_unsafe_database_sidecars(&path)?;
        let connection = open_private_connection(&path)?;
        connection.busy_timeout(Duration::from_millis(5_000))?;
        let version = user_version(&connection)?;
        let object_count: i64 = connection.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if object_count == 0 {
            connection.execute_batch(SCHEMA_SQL)?;
        } else if version != SCHEMA_VERSION {
            return Err(StorageError::UnsupportedVersion {
                found: version,
                required: SCHEMA_VERSION,
            });
        } else {
            if !database_integrity_is_ok(&connection)? {
                return Err(StorageError::IntegrityCheckFailed);
            }
            if !schema_contract_matches(&connection)? {
                return Err(StorageError::IncompatibleSchema);
            }
        }
        configure_connection(&connection)?;
        connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        set_schema_version(&connection, 92)?;
        repair_private_database_files(&path)?;
        drop(connection);
        repair_private_database_files(&path)?;
        Ok(Self { path })
    }

    pub fn connect(&self) -> Result<Connection, StorageError> {
        prepare_database_file(&self.path)?;
        reject_unsafe_database_sidecars(&self.path)?;
        let connection = open_private_connection(&self.path)?;
        configure_connection(&connection)?;
        repair_private_database_files(&self.path)?;
        Ok(connection)
    }

    /// Irreversibly clears every logical v18 data table while preserving its
    /// schema, triggers, and indexes.
    ///
    /// The snapshot FTS table deliberately has no delete trigger in schema 18,
    /// so all three external-content FTS indexes are rebuilt inside the same
    /// transaction. Secure deletion plus a post-commit VACUUM prevents deleted
    /// captured text from remaining in ordinary SQLite free pages.
    pub fn delete_all_data(&self) -> Result<usize, StorageError> {
        purge_quarantine_incidents(&self.path)?;
        let mut connection = self.connect()?;
        connection.pragma_update(None, "secure_delete", "ON")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut deleted_rows = 0;

        // Delete dependent rows before their referenced parents. The remaining
        // v18 tables do not carry foreign keys, but keeping a fixed explicit
        // order also makes this operation auditable against LOGICAL_TABLES.
        for table in [
            "activity_events",
            "working_memory",
            "kg_relations",
            "chronicle",
            "index_entries",
            "kg_entities",
            "workflows",
            "nudges",
            "proactive_rules",
            "recording_events",
            "inline_rewrite_uses",
            "wiki_pages",
            "salient_flags",
            "chat_turns",
            "inline_outputs",
            "time_ledger",
            "time_rules",
            "style_notes",
            "snapshots",
        ] {
            deleted_rows += transaction.execute(&format!("DELETE FROM {table}"), [])?;
        }

        transaction.execute(
            "INSERT INTO snapshots_fts(snapshots_fts) VALUES('rebuild')",
            [],
        )?;
        transaction.execute("INSERT INTO index_fts(index_fts) VALUES('rebuild')", [])?;
        transaction.execute("INSERT INTO wiki_fts(wiki_fts) VALUES('rebuild')", [])?;
        transaction.execute(
            "DELETE FROM sqlite_sequence WHERE name IN (
                'activity_events', 'working_memory', 'index_entries',
                'recording_events', 'inline_rewrite_uses', 'chat_turns',
                'inline_outputs', 'time_ledger', 'time_rules', 'style_notes'
            )",
            [],
        )?;
        transaction.commit()?;

        connection.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE);
             VACUUM;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        drop(connection);
        repair_private_database_files(&self.path)?;
        Ok(deleted_rows)
    }

    /// Deletes sensitive data older than `cutoff`, expressed as Unix seconds.
    ///
    /// Summaries and other generated memory may combine many captures without
    /// retaining a complete row-level provenance map. When any capture expires,
    /// those generated stores are invalidated as a unit so expired text cannot
    /// survive indirectly.
    pub fn prune_expired_data(&self, cutoff: i64) -> Result<RetentionPruneReport, StorageError> {
        // A quarantined database cannot prove row-level ages safely. Any
        // finite retention policy therefore removes every quarantine copy
        // before reporting that expiration has completed.
        purge_quarantine_incidents(&self.path)?;
        let mut connection = self.connect()?;
        connection.pragma_update(None, "secure_delete", "ON")?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired_snapshots: usize = transaction.query_row(
            "SELECT count(*) FROM snapshots WHERE last_seen_at < ?1",
            [cutoff],
            |row| row.get(0),
        )?;
        let expired_source_rows: usize = transaction.query_row(
            "SELECT
                 (SELECT count(*) FROM snapshots WHERE last_seen_at < ?1) +
                 (SELECT count(*) FROM activity_events WHERE last_seen_at < ?1) +
                 (SELECT count(*) FROM chat_turns WHERE created_at < ?1) +
                 (SELECT count(*) FROM time_ledger WHERE hour_ts < ?1)",
            [cutoff],
            |row| row.get(0),
        )?;
        let mut deleted_rows = 0;

        deleted_rows += transaction.execute(
            "DELETE FROM activity_events
             WHERE last_seen_at < ?1
                OR snapshot_id IN (SELECT snapshot_id FROM snapshots WHERE last_seen_at < ?1)",
            [cutoff],
        )?;
        deleted_rows += transaction.execute(
            "DELETE FROM working_memory
             WHERE added_at < ?1
                OR snapshot_id IN (SELECT snapshot_id FROM snapshots WHERE last_seen_at < ?1)",
            [cutoff],
        )?;
        deleted_rows += transaction.execute(
            "DELETE FROM kg_relations
             WHERE source_snapshot_id IN (
                 SELECT snapshot_id FROM snapshots WHERE last_seen_at < ?1
             )",
            [cutoff],
        )?;
        deleted_rows += transaction.execute(
            "DELETE FROM salient_flags
             WHERE created_at < ?1
                OR snapshot_id IN (SELECT snapshot_id FROM snapshots WHERE last_seen_at < ?1)",
            [cutoff],
        )?;

        deleted_rows += transaction.execute(
            "DELETE FROM recording_events WHERE ts_ms < ?1",
            [cutoff.saturating_mul(1_000)],
        )?;
        deleted_rows += transaction.execute(
            "DELETE FROM inline_rewrite_uses WHERE last_used_at < ?1",
            [cutoff],
        )?;
        deleted_rows +=
            transaction.execute("DELETE FROM chat_turns WHERE created_at < ?1", [cutoff])?;
        deleted_rows +=
            transaction.execute("DELETE FROM inline_outputs WHERE created_at < ?1", [cutoff])?;
        deleted_rows +=
            transaction.execute("DELETE FROM time_ledger WHERE hour_ts < ?1", [cutoff])?;
        deleted_rows +=
            transaction.execute("DELETE FROM style_notes WHERE updated_at < ?1", [cutoff])?;

        if expired_source_rows > 0 {
            for table in [
                "chronicle",
                "index_entries",
                "kg_relations",
                "kg_entities",
                "workflows",
                "wiki_pages",
                "salient_flags",
            ] {
                deleted_rows += transaction.execute(&format!("DELETE FROM {table}"), [])?;
            }
            deleted_rows +=
                transaction.execute("DELETE FROM nudges WHERE kind != 'scheduled_rule'", [])?;
            deleted_rows +=
                transaction.execute("DELETE FROM time_rules WHERE source != 'user'", [])?;
        }

        deleted_rows += transaction.execute(
            "DELETE FROM nudges
             WHERE max(
                 created_at,
                 COALESCE(sent_at, 0),
                 COALESCE(seen_at, 0),
                 COALESCE(dismissed_at, 0)
             ) < ?1",
            [cutoff],
        )?;
        deleted_rows +=
            transaction.execute("DELETE FROM snapshots WHERE last_seen_at < ?1", [cutoff])?;

        if deleted_rows > 0 {
            transaction.execute(
                "INSERT INTO snapshots_fts(snapshots_fts) VALUES('rebuild')",
                [],
            )?;
            transaction.execute("INSERT INTO index_fts(index_fts) VALUES('rebuild')", [])?;
            transaction.execute("INSERT INTO wiki_fts(wiki_fts) VALUES('rebuild')", [])?;
        }
        transaction.commit()?;

        if deleted_rows > 0 {
            connection.execute_batch(
                "PRAGMA wal_checkpoint(TRUNCATE);
                 VACUUM;
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )?;
        }
        drop(connection);
        repair_private_database_files(&self.path)?;
        Ok(RetentionPruneReport {
            deleted_rows,
            expired_snapshots,
            expired_source_rows,
        })
    }

    pub fn search_snapshots(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, StorageError> {
        let connection = self.connect()?;
        let query = fts_literal(query);
        let mut statement = connection.prepare(
            "SELECT s.snapshot_id, s.app, s.window_title, s.domain, s.captured_at,
                    substr(s.content, 1, 200),
                    bm25(snapshots_fts, 1.0, 0.25, 0.25, 0.5) AS rank
             FROM snapshots_fts
             JOIN snapshots s ON s.rowid = snapshots_fts.rowid
             WHERE snapshots_fts MATCH ?1
             ORDER BY rank ASC, s.captured_at DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![query, bounded_limit(limit, 30) as i64], |row| {
            Ok(SearchHit {
                snapshot_id: row.get(0)?,
                app: row.get(1)?,
                window_title: row.get(2)?,
                domain: row.get(3)?,
                captured_at: row.get(4)?,
                content_excerpt: row.get(5)?,
                score: -row.get::<_, f64>(6)?,
            })
        })?;
        collect_rows(rows)
    }

    /// Exports all snapshots in a stable order for an offline vector rebuild.
    pub fn export_snapshots(&self) -> Result<Vec<SnapshotExport>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT snapshot_id, content, last_seen_at
             FROM snapshots
             ORDER BY snapshot_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SnapshotExport {
                snapshot_id: row.get(0)?,
                content: row.get(1)?,
                last_seen_at: row.get(2)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn export_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<SnapshotExport>, StorageError> {
        let connection = self.connect()?;
        connection
            .query_row(
                "SELECT snapshot_id, content, last_seen_at
                 FROM snapshots
                 WHERE snapshot_id = ?1",
                [snapshot_id],
                |row| {
                    Ok(SnapshotExport {
                        snapshot_id: row.get(0)?,
                        content: row.get(1)?,
                        last_seen_at: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Resolves compact search projections in caller-provided order.
    pub fn search_hits_by_ids(&self, ids: &[String]) -> Result<Vec<SearchHit>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT snapshot_id, app, window_title, domain, captured_at,
                    substr(content, 1, 200)
             FROM snapshots
             WHERE snapshot_id = ?1",
        )?;
        let mut hits = Vec::new();
        for id in ids.iter().take(100) {
            if let Some(hit) = statement
                .query_row([id], |row| {
                    Ok(SearchHit {
                        snapshot_id: row.get(0)?,
                        app: row.get(1)?,
                        window_title: row.get(2)?,
                        domain: row.get(3)?,
                        captured_at: row.get(4)?,
                        content_excerpt: row.get(5)?,
                        score: 0.0,
                    })
                })
                .optional()?
            {
                hits.push(hit);
            }
        }
        Ok(hits)
    }

    pub fn snapshots(&self, ids: &[String]) -> Result<Vec<Snapshot>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT snapshot_id, content, app, window_title, url, domain, captured_at,
                    last_seen_at, duration_s, sighting_count, focused_name, focused_role,
                    focused_path
             FROM snapshots WHERE snapshot_id = ?1",
        )?;
        let mut snapshots = Vec::new();
        for id in ids.iter().take(100) {
            if let Some(snapshot) = statement
                .query_row([id], |row| snapshot_from_row(row, 0))
                .optional()?
            {
                snapshots.push(snapshot);
            }
        }
        Ok(snapshots)
    }

    pub fn recent_activity(
        &self,
        minutes: u32,
        limit: usize,
    ) -> Result<Vec<Activity>, StorageError> {
        let connection = self.connect()?;
        let cutoff = chrono::Utc::now().timestamp() - (i64::from(minutes.min(360)) * 60);
        let mut statement = connection.prepare(
            "SELECT event_id, snapshot_id, app, window_title, url, domain, started_at,
                    last_seen_at, duration_s, content_excerpt, focused_name, focused_role,
                    focused_path
             FROM activity_events
             WHERE last_seen_at >= ?1
             ORDER BY last_seen_at DESC
             LIMIT ?2",
        )?;
        let rows =
            statement.query_map(params![cutoff, bounded_limit(limit, 20) as i64], |row| {
                Ok(Activity {
                    event_id: row.get(0)?,
                    snapshot_id: row.get(1)?,
                    app: row.get(2)?,
                    window_title: row.get(3)?,
                    url: row.get(4)?,
                    domain: row.get(5)?,
                    started_at: row.get(6)?,
                    last_seen_at: row.get(7)?,
                    duration_s: row.get(8)?,
                    content_excerpt: row.get(9)?,
                    focused_name: row.get(10)?,
                    focused_role: row.get(11)?,
                    focused_path: row.get(12)?,
                })
            })?;
        let mut activity = collect_rows(rows)?;
        activity.reverse();
        Ok(activity)
    }

    pub fn working_memory(&self, limit: usize) -> Result<Vec<WorkingMemoryItem>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT wm.wm_id, wm.added_at, wm.relevance,
                    s.snapshot_id, s.content, s.app, s.window_title, s.url, s.domain,
                    s.captured_at, s.last_seen_at, s.duration_s, s.sighting_count,
                    s.focused_name, s.focused_role, s.focused_path
             FROM working_memory wm
             JOIN snapshots s ON s.snapshot_id = wm.snapshot_id
             ORDER BY wm.added_at DESC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([bounded_limit(limit, 200) as i64], |row| {
            Ok(WorkingMemoryItem {
                wm_id: row.get(0)?,
                added_at: row.get(1)?,
                relevance: row.get(2)?,
                snapshot: snapshot_from_row(row, 3)?,
            })
        })?;
        collect_rows(rows)
    }

    pub fn chronicle(&self, level: &str, period: &str) -> Result<Option<Chronicle>, StorageError> {
        let connection = self.connect()?;
        let result = connection
            .query_row(
                "SELECT chronicle_id, level, period_key, summary_text, snapshot_ids,
                        child_ids, token_count, generated_at, model_used, is_dirty
                 FROM chronicle WHERE level = ?1 AND period_key = ?2",
                params![level, period],
                |row| {
                    Ok(Chronicle {
                        chronicle_id: row.get(0)?,
                        level: row.get(1)?,
                        period_key: row.get(2)?,
                        summary_text: row.get(3)?,
                        snapshot_ids: row.get(4)?,
                        child_ids: row.get(5)?,
                        token_count: row.get(6)?,
                        generated_at: row.get(7)?,
                        model_used: row.get(8)?,
                        is_dirty: row.get(9)?,
                    })
                },
            )
            .optional()?;
        Ok(result)
    }

    pub fn snapshots_between(
        &self,
        from_timestamp: i64,
        to_timestamp: i64,
        limit: usize,
    ) -> Result<Vec<Snapshot>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT snapshot_id, content, app, window_title, url, domain, captured_at,
                    last_seen_at, duration_s, sighting_count, focused_name, focused_role,
                    focused_path
             FROM snapshots
             WHERE captured_at >= ?1 AND captured_at < ?2
             ORDER BY captured_at ASC, snapshot_id ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                from_timestamp,
                to_timestamp,
                bounded_limit(limit, 500) as i64
            ],
            |row| snapshot_from_row(row, 0),
        )?;
        collect_rows(rows)
    }

    pub fn has_snapshots_between(
        &self,
        from_timestamp: i64,
        to_timestamp: i64,
    ) -> Result<bool, StorageError> {
        self.connect()?
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM snapshots
                    WHERE captured_at >= ?1 AND captured_at < ?2
                    LIMIT 1
                 )",
                params![from_timestamp, to_timestamp],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .map_err(StorageError::from)
    }

    pub fn chronicles_by_keys(
        &self,
        level: &str,
        period_keys: &[String],
    ) -> Result<Vec<Chronicle>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT chronicle_id, level, period_key, summary_text, snapshot_ids,
                    child_ids, token_count, generated_at, model_used, is_dirty
             FROM chronicle
             WHERE level = ?1 AND period_key = ?2",
        )?;
        let mut chronicles = Vec::new();
        for period_key in period_keys.iter().take(500) {
            if let Some(chronicle) = statement
                .query_row(params![level, period_key], chronicle_from_row)
                .optional()?
            {
                chronicles.push(chronicle);
            }
        }
        Ok(chronicles)
    }

    pub fn user_messages_between(
        &self,
        from_timestamp: i64,
        to_timestamp: i64,
        limit: usize,
    ) -> Result<Vec<String>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT content
             FROM chat_turns
             WHERE role = 'user' AND created_at >= ?1 AND created_at < ?2
             ORDER BY created_at ASC, turn_id ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                from_timestamp,
                to_timestamp,
                bounded_limit(limit, 100) as i64
            ],
            |row| row.get(0),
        )?;
        collect_rows(rows)
    }

    pub fn unmatched_time_segments(
        &self,
        from_timestamp: i64,
        to_timestamp: i64,
        limit: usize,
    ) -> Result<Vec<UnmatchedTimeSegment>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT s.app, COALESCE(s.domain, ''), s.window_title,
                    sum(s.duration_s) / 60.0 AS minutes
             FROM snapshots s
             WHERE s.captured_at >= ?1 AND s.captured_at < ?2
               AND NOT EXISTS (
                   SELECT 1 FROM time_rules r
                   WHERE (r.app IS NULL OR r.app = s.app)
                     AND (r.domain IS NULL OR
                          s.domain = r.domain OR
                          (length(s.domain) > length(r.domain) AND
                           substr(s.domain, -(length(r.domain) + 1)) = ('.' || r.domain)))
                     AND (r.title_contains IS NULL OR
                          instr(lower(s.window_title), lower(r.title_contains)) > 0)
               )
             GROUP BY s.app, COALESCE(s.domain, ''), s.window_title
             HAVING minutes > 0
             ORDER BY minutes DESC, s.app ASC, s.window_title ASC
             LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![
                from_timestamp,
                to_timestamp,
                bounded_limit(limit, 100) as i64
            ],
            |row| {
                Ok(UnmatchedTimeSegment {
                    app: row.get(0)?,
                    domain: row.get(1)?,
                    window_title: row.get(2)?,
                    minutes: row.get(3)?,
                })
            },
        )?;
        collect_rows(rows)
    }

    pub fn known_projects(&self, limit: usize) -> Result<Vec<String>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT project FROM (
                 SELECT project, max(created_at) AS last_seen FROM time_rules GROUP BY project
                 UNION ALL
                 SELECT title AS project, max(last_seen) AS last_seen
                 FROM wiki_pages WHERE page_type = 'project' GROUP BY title
             )
             GROUP BY project
             ORDER BY max(last_seen) DESC, project ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map([bounded_limit(limit, 200) as i64], |row| row.get(0))?;
        collect_rows(rows)
    }

    /// Atomically installs one hourly chronicle and all derived memory. If the
    /// hour already exists, no side effects are applied.
    pub fn commit_hour_memory(&self, memory: &HourMemoryWrite) -> Result<bool, StorageError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if insert_chronicle(&transaction, &memory.chronicle)? == 0 {
            transaction.rollback()?;
            return Ok(false);
        }

        for page in &memory.wiki_pages {
            transaction.execute(
                "INSERT INTO wiki_pages
                 (slug, page_type, title, aliases, summary, body, links, snapshot_ids,
                  mention_count, first_seen, last_seen, is_dirty, updated_at, model_used)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, ?13)
                 ON CONFLICT(slug) DO UPDATE SET
                    page_type = excluded.page_type,
                    title = excluded.title,
                    aliases = excluded.aliases,
                    summary = excluded.summary,
                    body = excluded.body,
                    links = excluded.links,
                    snapshot_ids = excluded.snapshot_ids,
                    mention_count = excluded.mention_count,
                    first_seen = min(wiki_pages.first_seen, excluded.first_seen),
                    last_seen = max(wiki_pages.last_seen, excluded.last_seen),
                    is_dirty = 0,
                    updated_at = excluded.updated_at,
                    model_used = excluded.model_used",
                params![
                    page.slug,
                    page.page_type,
                    page.title,
                    page.aliases,
                    page.summary,
                    page.body,
                    page.links,
                    page.snapshot_ids,
                    page.mention_count,
                    page.first_seen,
                    page.last_seen,
                    page.updated_at,
                    page.model_used,
                ],
            )?;
            transaction.execute(
                "INSERT INTO kg_entities(entity_id, name, entity_type, first_seen, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(name, entity_type) DO UPDATE SET
                    first_seen = min(kg_entities.first_seen, excluded.first_seen),
                    last_seen = max(kg_entities.last_seen, excluded.last_seen)",
                params![
                    page.slug,
                    page.title,
                    page.page_type,
                    page.first_seen,
                    page.last_seen,
                ],
            )?;
        }

        // Wiki search topics and graph edges live in separate tables. Keep
        // those projections synchronized without a schema-changing trigger.
        for page in &memory.wiki_pages {
            transaction.execute(
                "INSERT INTO index_entries
                 (topic, entities, domain, snapshot_ids, created_at, last_updated_at)
                 VALUES (?1, ?2, NULL, ?3, ?4, ?4)
                 ON CONFLICT DO UPDATE SET
                    entities = excluded.entities,
                    snapshot_ids = excluded.snapshot_ids,
                    last_updated_at = excluded.last_updated_at",
                params![page.title, page.links, page.snapshot_ids, page.updated_at],
            )?;

            let subject_id = transaction
                .query_row(
                    "SELECT entity_id FROM kg_entities
                     WHERE name = ?1 COLLATE NOCASE AND entity_type = ?2
                     ORDER BY last_seen DESC LIMIT 1",
                    params![page.title, page.page_type],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            let Some(subject_id) = subject_id else {
                continue;
            };
            let source_snapshot_id = serde_json::from_str::<Vec<String>>(&page.snapshot_ids)
                .ok()
                .and_then(|ids| ids.into_iter().next());
            let source_snapshot_id = match source_snapshot_id {
                Some(snapshot_id)
                    if transaction.query_row(
                        "SELECT EXISTS(SELECT 1 FROM snapshots WHERE snapshot_id = ?1)",
                        [&snapshot_id],
                        |row| row.get::<_, bool>(0),
                    )? =>
                {
                    Some(snapshot_id)
                }
                _ => None,
            };
            let links = serde_json::from_str::<Vec<String>>(&page.links).unwrap_or_default();
            for link in links.into_iter().take(100) {
                let object_id = transaction
                    .query_row(
                        "SELECT entity_id FROM kg_entities
                         WHERE name = ?1 COLLATE NOCASE
                         ORDER BY last_seen DESC LIMIT 1",
                        [&link],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                let Some(object_id) = object_id.filter(|value| value != &subject_id) else {
                    continue;
                };
                let exists = transaction.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM kg_relations
                        WHERE subject_id = ?1 AND predicate = 'related_to'
                          AND object_id = ?2 AND valid_to IS NULL
                     )",
                    params![subject_id, object_id],
                    |row| row.get::<_, bool>(0),
                )?;
                if !exists {
                    transaction.execute(
                        "INSERT INTO kg_relations
                         (relation_id, subject_id, predicate, object_id, valid_from,
                          valid_to, source_snapshot_id)
                         VALUES (?1, ?2, 'related_to', ?3, ?4, NULL, ?5)",
                        params![
                            Uuid::now_v7().to_string(),
                            subject_id,
                            object_id,
                            page.updated_at,
                            source_snapshot_id,
                        ],
                    )?;
                }
            }
        }

        for flag in &memory.flags {
            transaction.execute(
                "INSERT OR IGNORE INTO salient_flags
                 (kind, text, snapshot_id, period_key, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'open', ?5)",
                params![
                    flag.kind,
                    flag.text,
                    flag.snapshot_id,
                    flag.period_key,
                    flag.created_at,
                ],
            )?;
            if matches!(flag.kind.as_str(), "commitment" | "blocker" | "question") {
                let flag_id = transaction.query_row(
                    "SELECT flag_id FROM salient_flags
                     WHERE period_key = ?1 AND kind = ?2 AND text = ?3",
                    params![flag.period_key, flag.kind, flag.text],
                    |row| row.get::<_, i64>(0),
                )?;
                let title = match flag.kind.as_str() {
                    "commitment" => "Commitment to revisit",
                    "blocker" => "Possible blocker",
                    _ => "Open question",
                };
                transaction.execute(
                    "INSERT OR IGNORE INTO nudges
                     (nudge_id, kind, dedupe_key, scheduled_for, title, body, deep_link,
                      status, created_at, meta_json)
                     VALUES (?1, 'contextual_nudge', ?2, ?3, ?4, ?5,
                             'woof://memory-hub/followups', 'pending', ?3, ?6)",
                    params![
                        Uuid::now_v7().to_string(),
                        format!("salient_flag:{flag_id}"),
                        flag.created_at,
                        title,
                        flag.text,
                        serde_json::json!({"flag_id": flag_id, "kind": flag.kind}).to_string(),
                    ],
                )?;
            }
        }

        for rule in &memory.time_rules {
            transaction.execute(
                "INSERT INTO time_rules
                 (project, app, domain, title_contains, source, created_at)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6
                 WHERE NOT EXISTS (
                    SELECT 1 FROM time_rules
                    WHERE project = ?1
                      AND app IS ?2
                      AND domain IS ?3
                      AND title_contains IS ?4
                 )",
                params![
                    rule.project,
                    rule.app,
                    rule.domain,
                    rule.title_contains,
                    rule.source,
                    rule.created_at,
                ],
            )?;
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn insert_chronicle_if_absent(
        &self,
        chronicle: &ChronicleWrite,
    ) -> Result<bool, StorageError> {
        Ok(insert_chronicle(&self.connect()?, chronicle)? > 0)
    }

    pub fn list_wiki(
        &self,
        page_type: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WikiSummary>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT slug, page_type, title, summary, mention_count, last_seen
             FROM wiki_pages
             WHERE (?1 IS NULL OR page_type = ?1)
             ORDER BY mention_count DESC, last_seen DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![page_type, bounded_limit(limit, 200) as i64],
            wiki_summary_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn wiki_page(&self, slug: &str) -> Result<Option<WikiPage>, StorageError> {
        let connection = self.connect()?;
        let page = connection
            .query_row(
                "SELECT slug, page_type, title, aliases, summary, body, links, snapshot_ids,
                        mention_count, first_seen, last_seen, is_dirty, updated_at, model_used
                 FROM wiki_pages WHERE slug = ?1",
                [slug],
                wiki_from_row,
            )
            .optional()?;
        Ok(page)
    }

    pub fn search_wiki(&self, query: &str, limit: usize) -> Result<Vec<WikiSummary>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT w.slug, w.page_type, w.title, w.summary, w.mention_count, w.last_seen
             FROM wiki_fts
             JOIN wiki_pages w ON w.rowid = wiki_fts.rowid
             WHERE wiki_fts MATCH ?1
             ORDER BY bm25(wiki_fts) ASC, w.last_seen DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![fts_literal(query), bounded_limit(limit, 100) as i64],
            wiki_summary_from_row,
        )?;
        collect_rows(rows)
    }

    pub fn time_rules(&self) -> Result<Vec<TimeRule>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT rule_id, project, app, domain, title_contains, source, created_at
             FROM time_rules ORDER BY created_at DESC, rule_id DESC",
        )?;
        let rows = statement.query_map([], time_rule_from_row)?;
        collect_rows(rows)
    }

    pub fn save_time_rule(
        &self,
        rule_id: Option<i64>,
        rule: &TimeRuleWrite,
    ) -> Result<Option<TimeRule>, StorageError> {
        let connection = self.connect()?;
        let rule_id = if let Some(rule_id) = rule_id {
            if connection.execute(
                "UPDATE time_rules
                 SET project = ?2, app = ?3, domain = ?4, title_contains = ?5,
                     source = ?6
                 WHERE rule_id = ?1",
                params![
                    rule_id,
                    rule.project,
                    rule.app,
                    rule.domain,
                    rule.title_contains,
                    rule.source,
                ],
            )? == 0
            {
                return Ok(None);
            }
            rule_id
        } else {
            connection.execute(
                "INSERT INTO time_rules
                 (project, app, domain, title_contains, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    rule.project,
                    rule.app,
                    rule.domain,
                    rule.title_contains,
                    rule.source,
                    rule.created_at,
                ],
            )?;
            connection.last_insert_rowid()
        };
        connection
            .query_row(
                "SELECT rule_id, project, app, domain, title_contains, source, created_at
                 FROM time_rules WHERE rule_id = ?1",
                [rule_id],
                time_rule_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn time_report(
        &self,
        from_timestamp: i64,
        to_timestamp: i64,
    ) -> Result<Vec<TimeReportRow>, StorageError> {
        use chrono::{Local, TimeZone};

        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT l.hour_ts, l.app, l.domain, l.title, l.seconds,
                    COALESCE(
                        (SELECT project FROM time_rules r
                         WHERE (r.app IS NULL OR r.app = l.app)
                           AND (r.domain IS NULL OR
                                r.domain = l.domain OR
                                (length(l.domain) > length(r.domain) AND
                                 substr(l.domain, -(length(r.domain) + 1)) = ('.' || r.domain)))
                           AND (r.title_contains IS NULL OR instr(lower(l.title), lower(r.title_contains)) > 0)
                         ORDER BY
                           (r.app IS NOT NULL) + (r.domain IS NOT NULL) + (r.title_contains IS NOT NULL) DESC,
                           r.rule_id DESC
                         LIMIT 1),
                        'Unclassified'
                    ) AS project
             FROM time_ledger l
             WHERE l.hour_ts >= ?1 AND l.hour_ts < ?2
             ORDER BY l.hour_ts ASC",
        )?;
        let rows = statement.query_map(params![from_timestamp, to_timestamp], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        let mut projects: BTreeMap<String, ProjectTimeAccumulator> = BTreeMap::new();
        for row in rows {
            let (hour_ts, app, domain, title, seconds, project) = row?;
            let day = Local
                .timestamp_opt(hour_ts, 0)
                .single()
                .map(|timestamp| timestamp.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "unknown".to_string());
            let accumulator = projects.entry(project).or_default();
            accumulator.seconds += seconds;
            *accumulator.by_day.entry(day).or_default() += seconds;
            *accumulator
                .segments
                .entry((app, domain, title))
                .or_default() += seconds;
        }
        let mut report = projects
            .into_iter()
            .map(|(project, accumulator)| {
                let mut top_segments = accumulator
                    .segments
                    .into_iter()
                    .map(|((app, domain, title), seconds)| TimeSegment {
                        app,
                        domain,
                        title,
                        seconds,
                    })
                    .collect::<Vec<_>>();
                top_segments.sort_by(|left, right| {
                    right
                        .seconds
                        .total_cmp(&left.seconds)
                        .then_with(|| left.app.cmp(&right.app))
                });
                top_segments.truncate(10);
                TimeReportRow {
                    project,
                    seconds: accumulator.seconds,
                    by_day: accumulator.by_day,
                    top_segments,
                }
            })
            .collect::<Vec<_>>();
        report.sort_by(|left, right| {
            match (
                left.project == "Unclassified",
                right.project == "Unclassified",
            ) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => right
                    .seconds
                    .total_cmp(&left.seconds)
                    .then_with(|| left.project.cmp(&right.project)),
            }
        });
        Ok(report)
    }

    pub fn followups(
        &self,
        status: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SalientFlag>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT flag_id, kind, text, snapshot_id, period_key, status, created_at
             FROM salient_flags
             WHERE kind IN ('followup', 'commitment', 'question')
               AND (?1 IS NULL OR status = ?1)
             ORDER BY created_at DESC
             LIMIT ?2",
        )?;
        let rows =
            statement.query_map(params![status, bounded_limit(limit, 200) as i64], |row| {
                Ok(SalientFlag {
                    flag_id: row.get(0)?,
                    kind: row.get(1)?,
                    text: row.get(2)?,
                    snapshot_id: row.get(3)?,
                    period_key: row.get(4)?,
                    status: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })?;
        collect_rows(rows)
    }

    pub fn set_followup_status(
        &self,
        flag_id: i64,
        status: &str,
        changed_at: i64,
    ) -> Result<bool, StorageError> {
        if flag_id <= 0 || !matches!(status, "resolved" | "dismissed") {
            return Ok(false);
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE salient_flags SET status = ?2
             WHERE flag_id = ?1
               AND kind IN ('followup', 'commitment', 'question')
               AND status = 'open'",
            params![flag_id, status],
        )?;
        if updated != 0 {
            transaction.execute(
                "UPDATE nudges
                 SET status = 'dismissed', dismissed_at = COALESCE(dismissed_at, ?2)
                 WHERE dedupe_key = ?1 AND status IN ('pending', 'ready')",
                params![format!("salient_flag:{flag_id}"), changed_at],
            )?;
        }
        transaction.commit()?;
        Ok(updated != 0)
    }

    pub fn ready_nudges(&self, now: i64, limit: usize) -> Result<Vec<Nudge>, StorageError> {
        self.materialize_due_rule_nudges(now, 500)?;
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE nudges SET status = 'ready'
             WHERE status = 'pending' AND scheduled_for <= ?1",
            [now],
        )?;
        let nudges = {
            let mut statement = transaction.prepare(
                "SELECT nudge_id, kind, dedupe_key, scheduled_for, title, body, deep_link,
                        status, created_at, sent_at, seen_at, dismissed_at, meta_json
                 FROM nudges
                 WHERE status = 'ready' AND scheduled_for <= ?1
                 ORDER BY (sent_at IS NOT NULL) ASC, scheduled_for ASC, created_at ASC
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(
                params![now, bounded_limit(limit, 50) as i64],
                nudge_from_row,
            )?;
            collect_rows(rows)?
        };
        transaction.commit()?;
        Ok(nudges)
    }

    pub fn ready_nudge(&self, nudge_id: &str) -> Result<Option<Nudge>, StorageError> {
        self.connect()?
            .query_row(
                "SELECT nudge_id, kind, dedupe_key, scheduled_for, title, body, deep_link,
                        status, created_at, sent_at, seen_at, dismissed_at, meta_json
                 FROM nudges
                 WHERE nudge_id = ?1 AND status = 'ready'",
                [nudge_id],
                nudge_from_row,
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn mark_nudge_delivered(
        &self,
        nudge_id: &str,
        delivered_at: i64,
    ) -> Result<bool, StorageError> {
        Ok(self.connect()?.execute(
            "UPDATE nudges
             SET sent_at = COALESCE(sent_at, ?2)
             WHERE nudge_id = ?1 AND status = 'ready'",
            params![nudge_id, delivered_at],
        )? > 0)
    }

    pub fn mark_nudge_seen(&self, nudge_id: &str, seen_at: i64) -> Result<bool, StorageError> {
        Ok(self.connect()?.execute(
            "UPDATE nudges
             SET status = 'seen', seen_at = ?2, sent_at = COALESCE(sent_at, ?2)
             WHERE nudge_id = ?1 AND status = 'ready'",
            params![nudge_id, seen_at],
        )? > 0)
    }

    pub fn dismiss_nudge(&self, nudge_id: &str, dismissed_at: i64) -> Result<bool, StorageError> {
        Ok(self.connect()?.execute(
            "UPDATE nudges
             SET status = 'dismissed', dismissed_at = ?2,
                 sent_at = COALESCE(sent_at, ?2)
             WHERE nudge_id = ?1 AND status = 'ready'",
            params![nudge_id, dismissed_at],
        )? > 0)
    }

    /// Materializes due proactive rules as deduplicated pending nudges and
    /// advances `last_fired_at` in the same transaction. Re-running this method
    /// for the same wall-clock slot is idempotent.
    pub fn materialize_due_rule_nudges(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<usize, StorageError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rules = {
            let mut statement = transaction.prepare(
                "SELECT rule_id, label, prompt, schedule_kind, days_of_week, hour, minute,
                        interval_minutes, timezone, enabled, created_at, updated_at,
                        last_fired_at, fire_at
                 FROM proactive_rules
                 WHERE enabled = 1
                 ORDER BY updated_at ASC",
            )?;
            let rows = statement.query_map([], proactive_rule_from_row)?;
            collect_rows(rows)?
        };

        let mut inserted = 0;
        let mut processed_due = 0;
        let due_limit = bounded_limit(limit, MAX_PROACTIVE_RULES as usize);
        for rule in rules {
            let Some(rule_id) = rule.rule_id.as_deref() else {
                continue;
            };
            let Some(slot) = due_rule_slot(&rule, now) else {
                continue;
            };
            if processed_due >= due_limit {
                break;
            }
            processed_due += 1;
            let dedupe_key = format!("scheduled_rule:{rule_id}:{slot}");
            let nudge_id = Uuid::now_v7().to_string();
            let deep_link = reminder_chat_deep_link(&rule.prompt);
            let meta_json = serde_json::json!({
                "rule_id": rule_id,
                "schedule_kind": rule.schedule_kind,
                "scheduled_slot": slot,
            })
            .to_string();
            inserted += transaction.execute(
                "INSERT OR IGNORE INTO nudges
                 (nudge_id, kind, dedupe_key, scheduled_for, title, body, deep_link,
                  status, created_at, meta_json)
                 VALUES (?1, 'scheduled_rule', ?2, ?3, ?4, ?5, ?6,
                         'pending', ?7, ?8)",
                params![
                    nudge_id,
                    dedupe_key,
                    slot,
                    rule.label,
                    rule.prompt,
                    deep_link,
                    now,
                    meta_json,
                ],
            )?;
            transaction.execute(
                "UPDATE proactive_rules
                 SET last_fired_at = max(COALESCE(last_fired_at, ?2), ?2), updated_at = max(updated_at, ?3)
                 WHERE rule_id = ?1",
                params![rule_id, slot, now],
            )?;
        }
        transaction.commit()?;
        Ok(inserted)
    }

    /// Moves due pending nudges into the notification-ready state. This is
    /// intentionally separate from reading so the daemon's scheduler can make
    /// progress even when no UI is open.
    pub fn promote_due_nudges(&self, now: i64) -> Result<usize, StorageError> {
        Ok(self.connect()?.execute(
            "UPDATE nudges SET status = 'ready'
             WHERE status = 'pending' AND scheduled_for <= ?1",
            [now],
        )?)
    }

    pub fn proactive_rules(&self) -> Result<Vec<ProactiveRule>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT rule_id, label, prompt, schedule_kind, days_of_week, hour, minute,
                    interval_minutes, timezone, enabled, created_at, updated_at,
                    last_fired_at, fire_at
             FROM proactive_rules
             ORDER BY enabled DESC, updated_at DESC",
        )?;
        let rows = statement.query_map([], proactive_rule_from_row)?;
        collect_rows(rows)
    }

    pub fn save_proactive_rule(&self, rule: ProactiveRule) -> Result<ProactiveRule, StorageError> {
        if rule.timezone != "local" {
            return Err(StorageError::UnsupportedReminderTimezone);
        }
        let weekdays =
            canonical_weekdays(&rule.days_of_week).ok_or(StorageError::InvalidReminder)?;
        let valid = match rule.schedule_kind.as_str() {
            "once" => weekdays.is_empty() && rule.interval_minutes == 0 && rule.fire_at.is_some(),
            "daily" => {
                weekdays.is_empty()
                    && rule.interval_minutes == 0
                    && rule.fire_at.is_none()
                    && (0..=23).contains(&rule.hour)
                    && (0..=59).contains(&rule.minute)
            }
            "weekly" => {
                !weekdays.is_empty()
                    && rule.interval_minutes == 0
                    && rule.fire_at.is_none()
                    && (0..=23).contains(&rule.hour)
                    && (0..=59).contains(&rule.minute)
            }
            "interval" => {
                weekdays.is_empty()
                    && (5..=7 * 24 * 60).contains(&rule.interval_minutes)
                    && rule.fire_at.is_none()
            }
            _ => false,
        };
        if !valid
            || rule.label.trim().is_empty()
            || rule.prompt.trim().is_empty()
            || rule.label.chars().any(char::is_control)
            || rule.prompt.chars().any(char::is_control)
            || rule.label.chars().count() > 120
            || rule.prompt.chars().count() > 1_000
            || rule.prompt.len() > MAX_REMINDER_PROMPT_BYTES
        {
            return Err(StorageError::InvalidReminder);
        }
        let rule_id = rule
            .rule_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let connection = self.connect()?;
        let already_exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM proactive_rules WHERE rule_id = ?1)",
            [&rule_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !already_exists
            && connection.query_row("SELECT count(*) FROM proactive_rules", [], |row| {
                row.get::<_, i64>(0)
            })? >= MAX_PROACTIVE_RULES
        {
            return Err(StorageError::InvalidReminder);
        }
        connection.execute(
            "INSERT INTO proactive_rules
             (rule_id, label, prompt, schedule_kind, days_of_week, hour, minute,
              interval_minutes, timezone, enabled, created_at, updated_at,
              last_fired_at, fire_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(rule_id) DO UPDATE SET
                 label = excluded.label,
                 prompt = excluded.prompt,
                 schedule_kind = excluded.schedule_kind,
                 days_of_week = excluded.days_of_week,
                 hour = excluded.hour,
                 minute = excluded.minute,
                 interval_minutes = excluded.interval_minutes,
                 timezone = excluded.timezone,
                 enabled = excluded.enabled,
                 updated_at = excluded.updated_at,
                 last_fired_at = excluded.last_fired_at,
                 fire_at = excluded.fire_at",
            params![
                rule_id,
                rule.label,
                rule.prompt,
                rule.schedule_kind,
                rule.days_of_week,
                rule.hour,
                rule.minute,
                rule.interval_minutes,
                rule.timezone,
                i64::from(rule.enabled),
                rule.created_at,
                rule.updated_at,
                rule.last_fired_at,
                rule.fire_at,
            ],
        )?;
        connection
            .query_row(
                "SELECT rule_id, label, prompt, schedule_kind, days_of_week, hour, minute,
                        interval_minutes, timezone, enabled, created_at, updated_at,
                        last_fired_at, fire_at
                 FROM proactive_rules WHERE rule_id = ?1",
                [rule_id],
                proactive_rule_from_row,
            )
            .map_err(StorageError::from)
    }

    pub fn delete_proactive_rule(&self, rule_id: &str) -> Result<bool, StorageError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted =
            transaction.execute("DELETE FROM proactive_rules WHERE rule_id = ?1", [rule_id])? > 0;
        transaction.execute(
            "DELETE FROM nudges
             WHERE kind = 'scheduled_rule'
               AND CASE WHEN json_valid(meta_json)
                        THEN json_extract(meta_json, '$.rule_id')
                   END = ?1",
            [rule_id],
        )?;
        transaction.commit()?;
        Ok(deleted)
    }

    pub fn overview_stats(&self, since: i64) -> Result<OverviewStats, StorageError> {
        self.connect()?
            .query_row(
                "SELECT
                (SELECT count(*) FROM snapshots WHERE last_seen_at >= ?1),
                (SELECT count(*) FROM activity_events WHERE last_seen_at >= ?1),
                (SELECT COALESCE(sum(duration_s), 0) FROM activity_events WHERE last_seen_at >= ?1),
                (SELECT count(DISTINCT app) FROM activity_events WHERE last_seen_at >= ?1),
                (SELECT count(DISTINCT domain) FROM activity_events
                    WHERE last_seen_at >= ?1 AND domain IS NOT NULL AND domain <> ''),
                (SELECT count(*) FROM wiki_pages),
                (SELECT count(*) FROM salient_flags
                    WHERE kind IN ('followup','commitment','question') AND status = 'open'),
                (SELECT count(*) FROM workflows)",
                [since],
                |row| {
                    Ok(OverviewStats {
                        snapshots: row.get(0)?,
                        activity_events: row.get(1)?,
                        total_duration_s: row.get(2)?,
                        active_apps: row.get(3)?,
                        places: row.get(4)?,
                        wiki_pages: row.get(5)?,
                        open_followups: row.get(6)?,
                        workflows: row.get(7)?,
                    })
                },
            )
            .map_err(StorageError::from)
    }

    pub fn record_chat_turn(
        &self,
        thread_id: Option<&str>,
        role: &str,
        content: &str,
        created_at: i64,
    ) -> Result<i64, StorageError> {
        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO chat_turns(thread_id, role, content, created_at, consumed)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![thread_id, role, content, created_at],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub fn work_pattern_status(&self, limit: usize) -> Result<WorkPatternStatus, StorageError> {
        let connection = self.connect()?;
        let total = connection.query_row("SELECT count(*) FROM workflows", [], |row| row.get(0))?;
        let mut by_status = BTreeMap::new();
        {
            let mut statement =
                connection.prepare("SELECT status, count(*) FROM workflows GROUP BY status")?;
            for row in statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (status, count) = row?;
                by_status.insert(status, count);
            }
        }
        let recent = {
            let mut statement = connection.prepare(
                "SELECT workflow_id, name, excerpt, apps, frequency_label, body_json,
                        status, confidence, first_detected_at, last_detected_at
                 FROM workflows
                 WHERE status != 'dismissed'
                 ORDER BY last_detected_at DESC LIMIT ?1",
            )?;
            let rows = statement.query_map([bounded_limit(limit, 100) as i64], |row| {
                let apps_json: String = row.get(3)?;
                let observations_json: String = row.get(5)?;
                Ok(WorkflowSummary {
                    workflow_id: row.get(0)?,
                    name: row.get(1)?,
                    excerpt: row.get(2)?,
                    apps: serde_json::from_str(&apps_json).unwrap_or_default(),
                    frequency_label: row.get(4)?,
                    observations: serde_json::from_str(&observations_json).unwrap_or_default(),
                    status: row.get(6)?,
                    confidence: row.get(7)?,
                    first_detected_at: row.get(8)?,
                    last_detected_at: row.get(9)?,
                })
            })?;
            collect_rows(rows)?
        };
        Ok(WorkPatternStatus {
            total,
            by_status,
            recent,
        })
    }

    pub fn set_workflow_status(
        &self,
        workflow_id: &str,
        status: &str,
        changed_at: i64,
    ) -> Result<bool, StorageError> {
        if Uuid::parse_str(workflow_id)
            .ok()
            .is_none_or(|parsed| parsed.hyphenated().to_string() != workflow_id)
            || !matches!(status, "accepted" | "dismissed")
        {
            return Ok(false);
        }
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE workflows SET status = ?2, generated_at = max(generated_at, ?3)
             WHERE workflow_id = ?1
               AND status = 'proposed'",
            params![workflow_id, status, changed_at],
        )?;
        if updated != 0 {
            transaction.execute(
                "UPDATE nudges
                 SET status = 'dismissed', dismissed_at = COALESCE(dismissed_at, ?2)
                 WHERE dedupe_key = ?1 AND status IN ('pending', 'ready')",
                params![format!("workflow:{workflow_id}"), changed_at],
            )?;
        }
        transaction.commit()?;
        Ok(updated != 0)
    }

    pub fn record_inline_use(
        &self,
        app: &str,
        domain: &str,
        used_at: i64,
    ) -> Result<i64, StorageError> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO inline_rewrite_uses
             (app, domain, first_used_at, last_used_at, use_count)
             VALUES (?1, ?2, ?3, ?3, 1)
             ON CONFLICT(app, domain) DO UPDATE SET
                 last_used_at = max(last_used_at, excluded.last_used_at),
                 use_count = use_count + 1",
            params![app, domain, used_at],
        )?;
        transaction.execute(
            "INSERT INTO recording_events(session_started_at, ts_ms, kind, app)
             VALUES (?1, ?1, 'inline_rewrite', ?2)",
            params![used_at.saturating_mul(1_000), app],
        )?;
        let use_count = transaction.query_row(
            "SELECT use_count FROM inline_rewrite_uses WHERE app = ?1 AND domain = ?2",
            params![app, domain],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(use_count)
    }

    pub fn record_inline_output(
        &self,
        app: &str,
        domain: &str,
        instruction: &str,
        output: &str,
        created_at: i64,
    ) -> Result<InlineOutput, StorageError> {
        let connection = self.connect()?;
        connection.execute(
            "INSERT INTO inline_outputs(app, domain, instruction, output, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![app, domain, instruction, output, created_at],
        )?;
        Ok(InlineOutput {
            output_id: connection.last_insert_rowid(),
            app: app.to_string(),
            domain: domain.to_string(),
            instruction: instruction.to_string(),
            output: output.to_string(),
            created_at,
        })
    }

    pub fn similar_inline_outputs(
        &self,
        app: &str,
        domain: &str,
        instruction: &str,
        limit: usize,
    ) -> Result<Vec<InlineOutput>, StorageError> {
        let connection = self.connect()?;
        let mut statement = connection.prepare(
            "SELECT output_id, app, domain, instruction, output, created_at
             FROM inline_outputs
             WHERE (?1 = '' OR app = ?1)
               AND (?2 = '' OR domain = ?2)
               AND (?3 = '' OR instruction = ?3 OR instruction LIKE '%' || ?3 || '%')
             ORDER BY (instruction = ?3) DESC, created_at DESC
             LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![app, domain, instruction, bounded_limit(limit, 50) as i64],
            inline_output_from_row,
        )?;
        collect_rows(rows)
    }

    /// Atomically persists a redacted Accessibility capture and its activity indexes.
    pub fn record_capture(
        &self,
        capture: &CaptureRecord,
        working_memory_capacity: usize,
    ) -> Result<String, StorageError> {
        let snapshot_id = capture
            .snapshot_id
            .clone()
            .unwrap_or_else(|| Uuid::now_v7().to_string());
        let mut connection = self.connect()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous_duration = transaction
            .query_row(
                "SELECT duration_s FROM snapshots WHERE snapshot_id = ?1",
                [&snapshot_id],
                |row| row.get::<_, f64>(0),
            )
            .optional()?
            .unwrap_or_default();
        transaction.execute(
            "INSERT INTO snapshots
             (snapshot_id, content, app, window_title, url, domain, captured_at, last_seen_at,
              duration_s, sighting_count, focused_name, focused_role, focused_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12)
             ON CONFLICT(snapshot_id) DO UPDATE SET
                 content = excluded.content,
                 app = excluded.app,
                 window_title = excluded.window_title,
                 url = excluded.url,
                 domain = excluded.domain,
                 last_seen_at = max(snapshots.last_seen_at, excluded.last_seen_at),
                 duration_s = max(snapshots.duration_s, excluded.duration_s),
                 sighting_count = snapshots.sighting_count + 1,
                 focused_name = excluded.focused_name,
                 focused_role = excluded.focused_role,
                 focused_path = excluded.focused_path",
            params![
                snapshot_id,
                capture.content,
                capture.app,
                capture.window_title,
                capture.url,
                capture.domain,
                capture.captured_at,
                capture.last_seen_at,
                capture.duration_s,
                capture.focused_name,
                capture.focused_role,
                capture.focused_path,
            ],
        )?;

        let latest_event = transaction
            .query_row(
                "SELECT event_id FROM activity_events
                 WHERE snapshot_id = ?1
                 ORDER BY event_id DESC LIMIT 1",
                [&snapshot_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(event_id) = latest_event {
            transaction.execute(
                "UPDATE activity_events SET
                    app = ?2, window_title = ?3, url = ?4, domain = ?5,
                    last_seen_at = max(last_seen_at, ?6),
                    duration_s = max(duration_s, ?7),
                    content_excerpt = ?8,
                    focused_name = ?9, focused_role = ?10, focused_path = ?11
                 WHERE event_id = ?1",
                params![
                    event_id,
                    capture.app,
                    capture.window_title,
                    capture.url,
                    capture.domain,
                    capture.last_seen_at,
                    capture.duration_s,
                    tail_excerpt(&capture.content, 500),
                    capture.focused_name,
                    capture.focused_role,
                    capture.focused_path,
                ],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO activity_events
                 (snapshot_id, app, window_title, url, domain, started_at, last_seen_at,
                  duration_s, content_excerpt, focused_name, focused_role, focused_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    snapshot_id,
                    capture.app,
                    capture.window_title,
                    capture.url,
                    capture.domain,
                    capture.captured_at,
                    capture.last_seen_at,
                    capture.duration_s,
                    tail_excerpt(&capture.content, 500),
                    capture.focused_name,
                    capture.focused_role,
                    capture.focused_path,
                ],
            )?;
        }

        let existing_wm = transaction
            .query_row(
                "SELECT wm_id FROM working_memory WHERE snapshot_id = ?1
                 ORDER BY wm_id DESC LIMIT 1",
                [&snapshot_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(wm_id) = existing_wm {
            transaction.execute(
                "UPDATE working_memory SET added_at = ?2, relevance = 1.0 WHERE wm_id = ?1",
                params![wm_id, capture.last_seen_at],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO working_memory(snapshot_id, added_at, relevance)
                 VALUES (?1, ?2, 1.0)",
                params![snapshot_id, capture.last_seen_at],
            )?;
        }
        let capacity = working_memory_capacity.clamp(1, 10_000);
        transaction.execute(
            "DELETE FROM working_memory
             WHERE wm_id IN (
                 SELECT wm_id FROM working_memory
                 ORDER BY relevance DESC, added_at DESC
                 LIMIT -1 OFFSET ?1
             )",
            [capacity as i64],
        )?;
        let duration_delta = (capture.duration_s.max(0.0) - previous_duration.max(0.0)).max(0.0);
        record_time_ledger_delta(&transaction, capture, duration_delta)?;
        if let Some(workflow) = detect_workflow(&transaction, capture, &snapshot_id)? {
            if workflow.created {
                transaction.execute(
                    "INSERT OR IGNORE INTO nudges
                     (nudge_id, kind, dedupe_key, scheduled_for, title, body, deep_link,
                      status, created_at, meta_json)
                     VALUES (?1, 'contextual_nudge', ?2, ?3,
                             'Recurring work pattern noticed', ?4, 'woof://memory-hub/workflows',
                             'pending', ?3, ?5)",
                    params![
                        Uuid::now_v7().to_string(),
                        format!("workflow:{}", workflow.workflow_id),
                        capture.last_seen_at,
                        format!("Woof noticed a recurring pattern: {}", workflow.name),
                        serde_json::json!({"workflow_id": workflow.workflow_id}).to_string(),
                    ],
                )?;
            }
        }
        transaction.commit()?;
        Ok(snapshot_id)
    }
}

struct DetectedWorkflow {
    workflow_id: String,
    name: String,
    created: bool,
}

fn record_time_ledger_delta(
    connection: &Connection,
    capture: &CaptureRecord,
    seconds: f64,
) -> Result<(), StorageError> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Ok(());
    }
    let mut remaining = seconds;
    let mut cursor = capture.last_seen_at as f64;
    let mut buckets = 0usize;
    while remaining > f64::EPSILON && buckets < 10_000 {
        let probe = (cursor.ceil() as i64).saturating_sub(1);
        let hour_ts = local_hour_bucket(probe);
        let available = (cursor - hour_ts as f64).max(f64::EPSILON);
        let segment_seconds = remaining.min(available);
        record_time_ledger_bucket(connection, capture, hour_ts, segment_seconds)?;
        remaining -= segment_seconds;
        cursor -= segment_seconds;
        buckets += 1;
    }
    if remaining > f64::EPSILON {
        record_time_ledger_bucket(
            connection,
            capture,
            local_hour_bucket((cursor.ceil() as i64).saturating_sub(1)),
            remaining,
        )?;
    }
    Ok(())
}

fn record_time_ledger_bucket(
    connection: &Connection,
    capture: &CaptureRecord,
    hour_ts: i64,
    seconds: f64,
) -> Result<(), StorageError> {
    connection.execute(
        "INSERT INTO time_ledger(hour_ts, app, domain, title, seconds)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(hour_ts, app, domain, title) DO UPDATE SET
            seconds = time_ledger.seconds + excluded.seconds",
        params![
            hour_ts,
            capture.app,
            capture.domain.as_deref().unwrap_or_default(),
            capture.window_title,
            seconds,
        ],
    )?;
    Ok(())
}

fn local_hour_bucket(timestamp: i64) -> i64 {
    let offset = Local
        .timestamp_opt(timestamp, 0)
        .single()
        .map(|local| i64::from(local.offset().local_minus_utc()))
        .unwrap_or_default();
    hour_bucket_for_offset(timestamp, offset)
}

fn hour_bucket_for_offset(timestamp: i64, offset_seconds: i64) -> i64 {
    timestamp
        .saturating_add(offset_seconds)
        .div_euclid(3_600)
        .saturating_mul(3_600)
        .saturating_sub(offset_seconds)
}

fn detect_workflow(
    connection: &Connection,
    capture: &CaptureRecord,
    _snapshot_id: &str,
) -> Result<Option<DetectedWorkflow>, StorageError> {
    let domain = capture.domain.as_deref().unwrap_or_default();
    let (occurrences, distinct_days, first_seen, last_seen): (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT count(DISTINCT snapshot_id), count(DISTINCT started_at / 86400),
                    min(started_at), max(last_seen_at)
             FROM activity_events
             WHERE app = ?1 AND COALESCE(domain, '') = ?2 AND window_title = ?3
               AND last_seen_at >= ?4",
            params![
                capture.app,
                domain,
                capture.window_title,
                capture.last_seen_at.saturating_sub(14 * 86_400),
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let observation_span = last_seen.saturating_sub(first_seen);
    if occurrences < 3 || (distinct_days < 2 && observation_span < 3_600) {
        return Ok(None);
    }

    let observations = {
        let mut statement = connection.prepare(
            "SELECT snapshot_id, app, domain, window_title, started_at, last_seen_at, duration_s
             FROM activity_events
             WHERE app = ?1 AND COALESCE(domain, '') = ?2 AND window_title = ?3
               AND last_seen_at >= ?4
             GROUP BY snapshot_id
             ORDER BY max(last_seen_at) DESC
             LIMIT 8",
        )?;
        let rows = statement.query_map(
            params![
                capture.app,
                domain,
                capture.window_title,
                capture.last_seen_at.saturating_sub(14 * 86_400),
            ],
            |row| {
                Ok(WorkflowObservation {
                    snapshot_id: row.get(0)?,
                    app: row.get(1)?,
                    domain: row.get(2)?,
                    window_title: row.get(3)?,
                    started_at: row.get(4)?,
                    last_seen_at: row.get(5)?,
                    duration_s: row.get(6)?,
                })
            },
        )?;
        let mut observations = collect_rows(rows)?;
        observations.reverse();
        observations
    };
    let apps = observations
        .iter()
        .map(|observation| observation.app.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let mut context_parts = vec![capture.app.as_str()];
    if !domain.is_empty() {
        context_parts.push(domain);
    }
    if !capture.window_title.trim().is_empty() {
        context_parts.push(capture.window_title.trim());
    }
    let context = context_parts.join(" · ");
    let name = truncate_chars(&context, 240);
    let existing = connection
        .query_row(
            "SELECT workflow_id FROM workflows WHERE lower(name) = lower(?1)",
            [&name],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let workflow_id = existing
        .clone()
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let confidence =
        (0.5 + (occurrences.min(8) as f64 * 0.045) + (distinct_days.min(4) as f64 * 0.025))
            .min(0.95);
    let apps = serde_json::to_string(&apps).unwrap_or_else(|_| "[]".to_string());
    let body_json = serde_json::to_string(&observations).unwrap_or_else(|_| "[]".to_string());
    let frequency_label = if distinct_days >= 2 {
        format!("{occurrences} recurrences across {distinct_days} days")
    } else {
        let hours = ((observation_span + 3_599) / 3_600).max(1);
        format!("{occurrences} recurrences across {hours} hours")
    };
    connection.execute(
        "INSERT INTO workflows
         (workflow_id, name, excerpt, apps, frequency_label, body_json, status, source,
          confidence, detected_at_level, first_detected_at, last_detected_at,
          generated_at, source_from, source_to)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'proposed', 'detected', ?7, 'capture',
                 ?8, ?9, ?9, ?10, ?11)
         ON CONFLICT DO UPDATE SET
            excerpt = excluded.excerpt,
            apps = excluded.apps,
            frequency_label = excluded.frequency_label,
            body_json = excluded.body_json,
            confidence = max(workflows.confidence, excluded.confidence),
            first_detected_at = min(workflows.first_detected_at, excluded.first_detected_at),
            last_detected_at = max(workflows.last_detected_at, excluded.last_detected_at),
            generated_at = excluded.generated_at,
            source_from = excluded.source_from,
            source_to = excluded.source_to",
        params![
            workflow_id,
            name,
            truncate_chars(&capture.window_title, 500),
            apps,
            frequency_label,
            body_json,
            confidence,
            first_seen,
            last_seen,
            first_seen.to_string(),
            last_seen.to_string(),
        ],
    )?;
    Ok(Some(DetectedWorkflow {
        workflow_id,
        name,
        created: existing.is_none(),
    }))
}

fn truncate_chars(value: &str, maximum: usize) -> String {
    if value.chars().count() <= maximum {
        return value.to_string();
    }
    value.chars().take(maximum).collect()
}

#[derive(Default)]
struct ProjectTimeAccumulator {
    seconds: f64,
    by_day: BTreeMap<String, f64>,
    segments: BTreeMap<(String, String, String), f64>,
}

fn reminder_chat_deep_link(prompt: &str) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("prompt", prompt);
    format!("woof://chat?{}", query.finish())
}

fn due_rule_slot(rule: &ProactiveRule, now: i64) -> Option<i64> {
    if !rule.enabled || rule.timezone != "local" {
        return None;
    }
    match rule.schedule_kind.as_str() {
        "once" => {
            let fire_at = rule.fire_at?;
            return (fire_at <= now && rule.last_fired_at.is_none_or(|last| last < fire_at))
                .then_some(fire_at);
        }
        "interval" => {
            if !(5..=7 * 24 * 60).contains(&rule.interval_minutes)
                || rule.fire_at.is_some()
                || !rule.days_of_week.is_empty()
            {
                return None;
            }
            let interval = rule.interval_minutes.checked_mul(60)?;
            let anchor = rule.last_fired_at.unwrap_or(rule.created_at);
            let elapsed = now.saturating_sub(anchor);
            if elapsed < interval {
                return None;
            }
            let slot = anchor.saturating_add((elapsed / interval).saturating_mul(interval));
            return rule
                .last_fired_at
                .is_none_or(|last| last < slot)
                .then_some(slot);
        }
        "daily" | "weekly" => {}
        _ => return None,
    }

    if rule.fire_at.is_some()
        || rule.interval_minutes != 0
        || !(0..=23).contains(&rule.hour)
        || !(0..=59).contains(&rule.minute)
    {
        return None;
    }
    let weekdays = canonical_weekdays(&rule.days_of_week)?;
    if (rule.schedule_kind == "daily" && !weekdays.is_empty())
        || (rule.schedule_kind == "weekly" && weekdays.is_empty())
    {
        return None;
    }
    let local_now = Local.timestamp_opt(now, 0).single()?;
    let iso_weekday = local_now.weekday().num_days_from_monday() + 1;
    if rule.schedule_kind == "weekly" && !weekdays.contains(&iso_weekday) {
        return None;
    }
    let local_slot = Local
        .from_local_datetime(&local_now.date_naive().and_hms_opt(
            rule.hour as u32,
            rule.minute as u32,
            0,
        )?)
        .earliest()?;
    let slot = local_slot.timestamp();
    (slot <= now && rule.last_fired_at.is_none_or(|last| last < slot)).then_some(slot)
}

fn canonical_weekdays(specification: &str) -> Option<Vec<u32>> {
    if specification.is_empty() {
        return Some(Vec::new());
    }
    let mut days = Vec::new();
    let mut previous = 0;
    for value in specification.split(',') {
        let day = value.parse::<u32>().ok()?;
        if !(1..=7).contains(&day) || day <= previous {
            return None;
        }
        days.push(day);
        previous = day;
    }
    Some(days)
}

fn recoverable_startup_reason(error: &StorageError) -> Option<StorageRecoveryReason> {
    match error {
        StorageError::UnsupportedVersion { .. } => Some(StorageRecoveryReason::UnsupportedVersion),
        StorageError::IntegrityCheckFailed => Some(StorageRecoveryReason::Corrupt),
        StorageError::IncompatibleSchema => Some(StorageRecoveryReason::IncompatibleSchema),
        StorageError::Sqlite(rusqlite::Error::SqliteFailure(sqlite_error, _))
            if matches!(
                sqlite_error.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            ) =>
        {
            Some(StorageRecoveryReason::Corrupt)
        }
        _ => None,
    }
}

fn database_integrity_is_ok(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let mut statement = connection.prepare("PRAGMA quick_check(1)")?;
    let mut rows = statement.query([])?;
    let Some(row) = rows.next()? else {
        return Ok(false);
    };
    let status = row.get::<_, String>(0)?;
    Ok(status == "ok" && rows.next()?.is_none())
}

fn schema_contract_matches(connection: &Connection) -> Result<bool, rusqlite::Error> {
    let canonical = Connection::open_in_memory()?;
    canonical.execute_batch(SCHEMA_SQL)?;

    let tables = schema_names(
        connection,
        "SELECT name FROM sqlite_master WHERE type = 'table'",
    )?;
    let canonical_tables = schema_names(
        &canonical,
        "SELECT name FROM sqlite_master WHERE type = 'table'",
    )?;
    if tables != canonical_tables {
        return Ok(false);
    }

    let views = schema_names(
        connection,
        "SELECT name FROM sqlite_master WHERE type = 'view'",
    )?;
    let canonical_views = schema_names(
        &canonical,
        "SELECT name FROM sqlite_master WHERE type = 'view'",
    )?;
    if views != canonical_views {
        return Ok(false);
    }

    let logical_tables = schema_names(
        connection,
        "SELECT name FROM sqlite_master
         WHERE type = 'table'
           AND name NOT LIKE 'sqlite_%'
           AND name NOT LIKE '%_fts'
           AND name NOT LIKE '%_fts_%'",
    )?;
    if logical_tables
        != LOGICAL_TABLES
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>()
    {
        return Ok(false);
    }

    let virtual_tables = schema_names(
        connection,
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name IN ('snapshots_fts', 'index_fts', 'wiki_fts')",
    )?;
    if virtual_tables
        != VIRTUAL_TABLES
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>()
    {
        return Ok(false);
    }

    let indexes = schema_names(
        connection,
        "SELECT name FROM sqlite_master
         WHERE type = 'index' AND name NOT LIKE 'sqlite_%'",
    )?;
    if indexes
        != NAMED_INDEXES
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>()
    {
        return Ok(false);
    }

    let triggers = schema_names(
        connection,
        "SELECT name FROM sqlite_master WHERE type = 'trigger'",
    )?;
    if triggers
        != SCHEMA_TRIGGERS
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>()
    {
        return Ok(false);
    }

    for table in LOGICAL_TABLES {
        let non_strict: i64 = connection.query_row(
            "SELECT count(*) FROM pragma_table_list
             WHERE \"schema\" = 'main' AND name = ?1 AND type = 'table' AND strict = 0",
            [table],
            |row| row.get(0),
        )?;
        if non_strict != 1 {
            return Ok(false);
        }
    }

    for (table, column) in [
        ("snapshots", "snapshot_id"),
        ("chronicle", "chronicle_id"),
        ("kg_entities", "entity_id"),
        ("kg_relations", "relation_id"),
        ("workflows", "workflow_id"),
        ("nudges", "nudge_id"),
        ("proactive_rules", "rule_id"),
        ("wiki_pages", "slug"),
    ] {
        let primary_key = connection
            .query_row(
                "SELECT type, \"notnull\", pk FROM pragma_table_info(?1) WHERE name = ?2",
                params![table, column],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        if primary_key
            .as_ref()
            .map(|(kind, not_null, key_order)| (kind.as_str(), *not_null, *key_order))
            != Some(("TEXT", 0, 1))
        {
            return Ok(false);
        }
    }

    for table in LOGICAL_TABLES.into_iter().chain(VIRTUAL_TABLES) {
        if table_column_contract(connection, table)? != table_column_contract(&canonical, table)?
            || foreign_key_contract(connection, table)? != foreign_key_contract(&canonical, table)?
            || schema_object_contract(connection, "table", table)?
                != schema_object_contract(&canonical, "table", table)?
        {
            return Ok(false);
        }
    }
    for index in NAMED_INDEXES {
        if index_contract(connection, index)? != index_contract(&canonical, index)?
            || schema_object_contract(connection, "index", index)?
                != schema_object_contract(&canonical, "index", index)?
        {
            return Ok(false);
        }
    }
    for trigger in SCHEMA_TRIGGERS {
        if schema_object_contract(connection, "trigger", trigger)?
            != schema_object_contract(&canonical, "trigger", trigger)?
        {
            return Ok(false);
        }
    }

    Ok(true)
}

fn schema_names(connection: &Connection, query: &str) -> Result<BTreeSet<String>, rusqlite::Error> {
    connection
        .prepare(query)?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()
}

type TableColumnContract = (i64, String, String, i64, Option<String>, i64, i64);

fn table_column_contract(
    connection: &Connection,
    table: &str,
) -> Result<Vec<TableColumnContract>, rusqlite::Error> {
    connection
        .prepare(
            "SELECT cid, name, type, \"notnull\", dflt_value, pk, hidden
             FROM pragma_table_xinfo(?1) ORDER BY cid",
        )?
        .query_map([table], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?
        .collect::<Result<_, _>>()
}

type ForeignKeyContract = (
    i64,
    i64,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
);

fn foreign_key_contract(
    connection: &Connection,
    table: &str,
) -> Result<Vec<ForeignKeyContract>, rusqlite::Error> {
    connection
        .prepare(
            "SELECT id, seq, \"table\", \"from\", \"to\", on_update, on_delete, \"match\"
             FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
        )?
        .query_map([table], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })?
        .collect::<Result<_, _>>()
}

type IndexContract = (i64, i64, Option<String>, i64, String, i64);

fn index_contract(
    connection: &Connection,
    index: &str,
) -> Result<Vec<IndexContract>, rusqlite::Error> {
    connection
        .prepare(
            "SELECT seqno, cid, name, desc, coll, key
             FROM pragma_index_xinfo(?1) ORDER BY seqno",
        )?
        .query_map([index], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?
        .collect::<Result<_, _>>()
}

fn schema_object_contract(
    connection: &Connection,
    object_type: &str,
    name: &str,
) -> Result<Option<String>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = ?1 AND name = ?2",
            params![object_type, name],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map(|sql| sql.flatten().map(|sql| normalize_schema_sql(&sql)))
}

fn normalize_schema_sql(sql: &str) -> String {
    let mut normalized = String::with_capacity(sql.len());
    let mut quoted_until = None;
    let mut characters = sql.chars().peekable();
    while let Some(character) = characters.next() {
        if let Some(delimiter) = quoted_until {
            normalized.push(character);
            if character == delimiter {
                if delimiter != ']' && characters.peek() == Some(&delimiter) {
                    normalized.push(characters.next().expect("peeked quote"));
                } else {
                    quoted_until = None;
                }
            }
            continue;
        }

        match character {
            '\'' | '"' | '`' => {
                normalized.push(character);
                quoted_until = Some(character);
            }
            '[' => {
                normalized.push(character);
                quoted_until = Some(']');
            }
            _ if character.is_ascii_whitespace() => {}
            _ => normalized.push(character.to_ascii_lowercase()),
        }
    }
    normalized
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct DatabaseFileIdentity {
    length: u64,
    modified_nanos: Option<u64>,
    device: u64,
    inode: u64,
}

fn database_file_identity(path: &Path) -> Result<Option<DatabaseFileIdentity>, StorageError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(path, error)),
    };
    validate_regular_database_file(path, &metadata)?;
    Ok(Some(DatabaseFileIdentity {
        length: metadata.len(),
        modified_nanos: metadata.modified().ok().and_then(|modified| {
            modified
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        }),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(not(unix))]
        device: 0,
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(not(unix))]
        inode: 0,
    }))
}

fn quarantine_database_family(
    database: &Path,
    expected_database_identity: &DatabaseFileIdentity,
    reason: StorageRecoveryReason,
    after_operation: &mut dyn FnMut(RecoveryOperation) -> Result<(), StorageError>,
) -> Result<PathBuf, StorageError> {
    if database_file_identity(database)?.as_ref() != Some(expected_database_identity) {
        return Err(StorageError::DatabaseChangedDuringRecovery);
    }
    let parent = database_parent(database)?;
    let quarantine_root = parent.join(DATABASE_QUARANTINE_DIRECTORY);
    if create_private_directory(&quarantine_root)? {
        after_operation(RecoveryOperation::QuarantineRootCreated)?;
    }
    sync_directory(parent)?;
    after_operation(RecoveryOperation::QuarantineRootParentSynced)?;
    let quarantine_directory = create_quarantine_directory(&quarantine_root, after_operation)?;

    let files = source_incident_files(database, expected_database_identity)?;
    let mut state = IncidentState {
        format_version: INCIDENT_FORMAT_VERSION,
        phase: IncidentPhase::Pending,
        created_at: chrono::Utc::now().timestamp(),
        reason,
        files,
    };
    write_incident_state(&quarantine_directory, &state, after_operation)?;

    for (index, file) in state.files.iter().enumerate() {
        let source = source_file_path(database, &file.suffix);
        let destination = incident_file_path(&quarantine_directory, &file.suffix);
        copy_private_file(&source, &destination, index, after_operation)?;
    }
    sync_directory(&quarantine_directory)?;
    after_operation(RecoveryOperation::CopiesDirectorySynced)?;
    validate_sources_and_copies(database, &quarantine_directory, &state.files)?;
    after_operation(RecoveryOperation::SourcesRevalidated)?;

    state.phase = IncidentPhase::Ready;
    write_incident_state(&quarantine_directory, &state, after_operation)?;
    match finalize_ready_incident(database, &quarantine_directory, &mut state, after_operation)? {
        ReadyFinalization::OriginalsRemoved => Ok(incident_file_path(&quarantine_directory, "")),
        ReadyFinalization::SourceChanged => Err(StorageError::DatabaseChangedDuringRecovery),
    }
}

fn reconcile_quarantine_incidents(
    database: &Path,
    after_operation: &mut dyn FnMut(RecoveryOperation) -> Result<(), StorageError>,
) -> Result<Option<StorageRecovery>, StorageError> {
    let parent = database_parent(database)?;
    let root = parent.join(DATABASE_QUARANTINE_DIRECTORY);
    let incidents = validated_incident_directories(&root)?;
    let mut completed_recovery = None;
    for incident in incidents {
        let state = read_incident_state(&incident)?;
        match state {
            Some(IncidentState {
                phase: IncidentPhase::Purging,
                ..
            }) => secure_purge_incident(&root, &incident)?,
            None
            | Some(IncidentState {
                phase: IncidentPhase::Pending,
                ..
            }) => {
                if database_file_identity(database)?.is_none() {
                    return Err(StorageError::UnexpectedQuarantineStructure);
                }
                secure_purge_incident(&root, &incident)?;
            }
            Some(mut state) if state.phase == IncidentPhase::Ready => {
                validate_incident_copies(&incident, &state)?;
                let finalization =
                    finalize_ready_incident(database, &incident, &mut state, after_operation)?;
                if finalization == ReadyFinalization::OriginalsRemoved {
                    completed_recovery = Some(StorageRecovery {
                        reason: state.reason,
                        quarantined_database_path: incident_file_path(&incident, ""),
                    });
                }
            }
            Some(state) => {
                validate_incident_copies(&incident, &state)?;
                remove_stale_state_temp(&incident)?;
            }
        }
    }
    Ok(completed_recovery)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadyFinalization {
    OriginalsRemoved,
    SourceChanged,
}

fn finalize_ready_incident(
    database: &Path,
    incident: &Path,
    state: &mut IncidentState,
    after_operation: &mut dyn FnMut(RecoveryOperation) -> Result<(), StorageError>,
) -> Result<ReadyFinalization, StorageError> {
    validate_incident_copies(incident, state)?;
    if !source_family_matches_incident(database, incident, &state.files)? {
        state.phase = IncidentPhase::Finalized;
        write_incident_state(incident, state, after_operation)?;
        return Ok(ReadyFinalization::SourceChanged);
    }

    for (index, file) in state.files.iter().enumerate().rev() {
        let source = source_file_path(database, &file.suffix);
        match database_file_identity(&source)? {
            Some(identity) if identity == file.source_identity => {
                fs::remove_file(&source).map_err(|error| io_error(&source, error))?;
                after_operation(RecoveryOperation::OriginalRemoved(index))?;
            }
            None => {}
            Some(_) => return Err(StorageError::DatabaseChangedDuringRecovery),
        }
    }
    let parent = database_parent(database)?;
    sync_directory(parent)?;
    after_operation(RecoveryOperation::SourceDirectorySynced)?;
    state.phase = IncidentPhase::Finalized;
    write_incident_state(incident, state, after_operation)?;
    Ok(ReadyFinalization::OriginalsRemoved)
}

fn source_incident_files(
    database: &Path,
    expected_database_identity: &DatabaseFileIdentity,
) -> Result<Vec<IncidentFile>, StorageError> {
    let mut files = Vec::with_capacity(DATABASE_SIDECAR_SUFFIXES.len() + 1);
    repair_private_mode(database)?;
    files.push(IncidentFile {
        suffix: String::new(),
        source_identity: expected_database_identity.clone(),
    });
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        let source = source_file_path(database, suffix);
        if let Some(source_identity) = database_file_identity(&source)? {
            repair_private_mode(&source)?;
            files.push(IncidentFile {
                suffix: suffix.to_string(),
                source_identity,
            });
        }
    }
    validate_incident_files(&files)?;
    Ok(files)
}

fn validate_sources_and_copies(
    database: &Path,
    incident: &Path,
    files: &[IncidentFile],
) -> Result<(), StorageError> {
    if !source_family_matches_manifest(database, files)? {
        return Err(StorageError::DatabaseChangedDuringRecovery);
    }
    for file in files {
        let source = source_file_path(database, &file.suffix);
        let copy = incident_file_path(incident, &file.suffix);
        let copy_identity =
            database_file_identity(&copy)?.ok_or(StorageError::UnexpectedQuarantineStructure)?;
        if copy_identity.length != file.source_identity.length || !files_equal(&source, &copy)? {
            return Err(StorageError::DatabaseChangedDuringRecovery);
        }
    }
    Ok(())
}

fn source_family_matches_manifest(
    database: &Path,
    files: &[IncidentFile],
) -> Result<bool, StorageError> {
    validate_incident_files(files)?;
    for file in files {
        let source = source_file_path(database, &file.suffix);
        if let Some(identity) = database_file_identity(&source)? {
            if identity != file.source_identity {
                return Ok(false);
            }
        }
    }
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        if files.iter().all(|file| file.suffix != suffix)
            && database_file_identity(&source_file_path(database, suffix))?.is_some()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn source_family_matches_incident(
    database: &Path,
    incident: &Path,
    files: &[IncidentFile],
) -> Result<bool, StorageError> {
    if !source_family_matches_manifest(database, files)? {
        return Ok(false);
    }
    for file in files {
        let source = source_file_path(database, &file.suffix);
        if database_file_identity(&source)?.is_some()
            && !files_equal(&source, &incident_file_path(incident, &file.suffix))?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn validate_incident_copies(incident: &Path, state: &IncidentState) -> Result<(), StorageError> {
    validate_incident_state(state)?;
    let entries = validated_incident_entries(incident)?;
    for file in &state.files {
        let copy = incident_file_path(incident, &file.suffix);
        let identity =
            database_file_identity(&copy)?.ok_or(StorageError::UnexpectedQuarantineStructure)?;
        if identity.length != file.source_identity.length {
            return Err(StorageError::UnexpectedQuarantineStructure);
        }
    }
    for entry in entries {
        let Some(name) = entry.file_name().and_then(|name| name.to_str()) else {
            return Err(StorageError::UnexpectedQuarantineStructure);
        };
        if matches!(name, INCIDENT_STATE_FILE | INCIDENT_STATE_TEMP_FILE) {
            continue;
        }
        if state
            .files
            .iter()
            .all(|file| incident_copy_name(&file.suffix) != name)
        {
            return Err(StorageError::UnexpectedQuarantineStructure);
        }
    }
    Ok(())
}

fn copy_private_file(
    source: &Path,
    destination: &Path,
    index: usize,
    after_operation: &mut dyn FnMut(RecoveryOperation) -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let mut input = open_private_file(source, false)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut output = options
        .open(destination)
        .map_err(|error| io_error(destination, error))?;
    after_operation(RecoveryOperation::CopyCreated(index))?;
    std::io::copy(&mut input, &mut output).map_err(|error| io_error(destination, error))?;
    after_operation(RecoveryOperation::CopyWritten(index))?;
    output
        .sync_all()
        .map_err(|error| io_error(destination, error))?;
    after_operation(RecoveryOperation::CopySynced(index))?;
    repair_private_mode(destination)
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, StorageError> {
    let mut left = open_private_file(left, false)?;
    let mut right = open_private_file(right, false)?;
    let mut left_buffer = [0_u8; 64 * 1024];
    let mut right_buffer = [0_u8; 64 * 1024];
    loop {
        let left_read = left
            .read(&mut left_buffer)
            .map_err(|error| io_error(Path::new("database source"), error))?;
        let right_read = right
            .read(&mut right_buffer)
            .map_err(|error| io_error(Path::new("database quarantine"), error))?;
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

fn write_incident_state(
    incident: &Path,
    state: &IncidentState,
    after_operation: &mut dyn FnMut(RecoveryOperation) -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    validate_incident_state(state)?;
    remove_stale_state_temp(incident)?;
    let mut bytes =
        serde_json::to_vec(state).map_err(|_| StorageError::UnexpectedQuarantineStructure)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_INCIDENT_STATE_BYTES {
        return Err(StorageError::UnexpectedQuarantineStructure);
    }
    let temporary = incident.join(INCIDENT_STATE_TEMP_FILE);
    let state_path = incident.join(INCIDENT_STATE_FILE);
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary)
        .map_err(|error| io_error(&temporary, error))?;
    after_operation(RecoveryOperation::StateTempCreated)?;
    file.write_all(&bytes)
        .map_err(|error| io_error(&temporary, error))?;
    after_operation(RecoveryOperation::StateTempWritten)?;
    file.sync_all()
        .map_err(|error| io_error(&temporary, error))?;
    after_operation(RecoveryOperation::StateTempSynced)?;
    drop(file);
    if let Ok(metadata) = fs::symlink_metadata(&state_path) {
        validate_regular_database_file(&state_path, &metadata)?;
    }
    fs::rename(&temporary, &state_path).map_err(|error| io_error(&state_path, error))?;
    after_operation(RecoveryOperation::StateRenamed)?;
    sync_directory(incident)?;
    after_operation(RecoveryOperation::StateDirectorySynced)?;
    repair_private_mode(&state_path)
}

fn read_incident_state(incident: &Path) -> Result<Option<IncidentState>, StorageError> {
    let path = incident.join(INCIDENT_STATE_FILE);
    match fs::symlink_metadata(&path) {
        Ok(metadata) => validate_regular_database_file(&path, &metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(&path, error)),
    }
    let bytes = read_private_file_bounded(&path, MAX_INCIDENT_STATE_BYTES)?;
    let state = serde_json::from_slice::<IncidentState>(&bytes)
        .map_err(|_| StorageError::UnexpectedQuarantineStructure)?;
    validate_incident_state(&state)?;
    Ok(Some(state))
}

fn validate_incident_state(state: &IncidentState) -> Result<(), StorageError> {
    if state.format_version != INCIDENT_FORMAT_VERSION {
        return Err(StorageError::UnexpectedQuarantineStructure);
    }
    validate_incident_files(&state.files)
}

fn validate_incident_files(files: &[IncidentFile]) -> Result<(), StorageError> {
    if files.is_empty() || files.len() > DATABASE_SIDECAR_SUFFIXES.len() + 1 {
        return Err(StorageError::UnexpectedQuarantineStructure);
    }
    let mut suffixes = BTreeSet::new();
    for file in files {
        if !file.suffix.is_empty() && !DATABASE_SIDECAR_SUFFIXES.contains(&file.suffix.as_str()) {
            return Err(StorageError::UnexpectedQuarantineStructure);
        }
        if !suffixes.insert(file.suffix.as_str()) {
            return Err(StorageError::UnexpectedQuarantineStructure);
        }
    }
    if !suffixes.contains("") {
        return Err(StorageError::UnexpectedQuarantineStructure);
    }
    Ok(())
}

fn validated_incident_directories(root: &Path) -> Result<Vec<PathBuf>, StorageError> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(root, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageError::UnexpectedQuarantineStructure);
    }
    ensure_private_dir(root)?;
    let mut incidents = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| io_error(root, error))? {
        if incidents.len() == MAX_QUARANTINE_INCIDENTS {
            return Err(StorageError::UnexpectedQuarantineStructure);
        }
        let entry = entry.map_err(|error| io_error(root, error))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
        let valid_name = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("incident-"))
            .is_some_and(|id| Uuid::parse_str(id).is_ok());
        if !valid_name || metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StorageError::UnexpectedQuarantineStructure);
        }
        ensure_private_dir(&path)?;
        validated_incident_entries(&path)?;
        incidents.push(path);
    }
    incidents.sort();
    Ok(incidents)
}

fn validated_incident_entries(incident: &Path) -> Result<Vec<PathBuf>, StorageError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(incident).map_err(|error| io_error(incident, error))? {
        let entry = entry.map_err(|error| io_error(incident, error))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StorageError::UnexpectedQuarantineStructure);
        }
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            return Err(StorageError::UnexpectedQuarantineStructure);
        };
        let valid_name = matches!(
            name.as_str(),
            INCIDENT_STATE_FILE | INCIDENT_STATE_TEMP_FILE
        ) || name == QUARANTINED_DATABASE_FILE
            || DATABASE_SIDECAR_SUFFIXES
                .iter()
                .any(|suffix| name == incident_copy_name(suffix));
        if !valid_name {
            return Err(StorageError::UnexpectedQuarantineStructure);
        }
        repair_private_mode(&path)?;
        entries.push(path);
    }
    entries.sort();
    Ok(entries)
}

fn remove_stale_state_temp(incident: &Path) -> Result<(), StorageError> {
    let temporary = incident.join(INCIDENT_STATE_TEMP_FILE);
    match fs::symlink_metadata(&temporary) {
        Ok(metadata) => {
            validate_regular_database_file(&temporary, &metadata)?;
            securely_remove_private_file(&temporary)?;
            sync_directory(incident)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(&temporary, error)),
    }
}

fn create_quarantine_directory(
    root: &Path,
    after_operation: &mut dyn FnMut(RecoveryOperation) -> Result<(), StorageError>,
) -> Result<PathBuf, StorageError> {
    for _ in 0..MAX_QUARANTINE_DIRECTORY_ATTEMPTS {
        let directory = root.join(format!("incident-{}", Uuid::new_v4().simple()));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&directory) {
            Ok(()) => {
                after_operation(RecoveryOperation::IncidentDirectoryCreated)?;
                ensure_private_dir(&directory)?;
                sync_directory(root)?;
                after_operation(RecoveryOperation::IncidentDirectorySynced)?;
                return Ok(directory);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(&directory, error)),
        }
    }
    Err(io_error(
        root,
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique database quarantine directory",
        ),
    ))
}

fn create_private_directory(path: &Path) -> Result<bool, StorageError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    match builder.create(path) {
        Ok(()) => {
            ensure_private_dir(path)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure_private_dir(path)?;
            Ok(false)
        }
        Err(error) => Err(io_error(path, error)),
    }
}

fn sync_directory(path: &Path) -> Result<(), StorageError> {
    let directory = File::open(path).map_err(|error| io_error(path, error))?;
    directory.sync_all().map_err(|error| io_error(path, error))
}

fn database_parent(database: &Path) -> Result<&Path, StorageError> {
    database.parent().ok_or_else(|| {
        io_error(
            database,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path has no parent",
            ),
        )
    })
}

fn source_file_path(database: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        database.to_path_buf()
    } else {
        database_sidecar_path(database, suffix)
    }
}

fn incident_copy_name(suffix: &str) -> String {
    format!("{QUARANTINED_DATABASE_FILE}{suffix}")
}

fn incident_file_path(incident: &Path, suffix: &str) -> PathBuf {
    incident.join(incident_copy_name(suffix))
}

fn open_private_file(path: &Path, writable: bool) -> Result<File, StorageError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    validate_regular_database_file(path, &metadata)?;
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).map_err(|error| io_error(path, error))?;
    let opened_metadata = file.metadata().map_err(|error| io_error(path, error))?;
    validate_regular_database_file(path, &opened_metadata)?;
    #[cfg(unix)]
    file.set_permissions(Permissions::from_mode(0o600))
        .map_err(|error| io_error(path, error))?;
    Ok(file)
}

fn securely_remove_private_file(path: &Path) -> Result<(), StorageError> {
    let mut file = open_private_file(path, true)?;
    let length = file
        .metadata()
        .map_err(|error| io_error(path, error))?
        .len();
    let zeros = vec![0_u8; SECURE_ERASE_BUFFER_BYTES];
    let mut remaining = length;
    while remaining > 0 {
        let bytes = usize::try_from(remaining.min(zeros.len() as u64)).unwrap_or(zeros.len());
        file.write_all(&zeros[..bytes])
            .map_err(|error| io_error(path, error))?;
        remaining = remaining.saturating_sub(bytes as u64);
    }
    file.sync_all().map_err(|error| io_error(path, error))?;
    file.set_len(0).map_err(|error| io_error(path, error))?;
    file.sync_all().map_err(|error| io_error(path, error))?;
    drop(file);
    fs::remove_file(path).map_err(|error| io_error(path, error))
}

fn secure_purge_incident(root: &Path, incident: &Path) -> Result<(), StorageError> {
    let entries = validated_incident_entries(incident)?;
    for entry in entries {
        securely_remove_private_file(&entry)?;
    }
    sync_directory(incident)?;
    fs::remove_dir(incident).map_err(|error| io_error(incident, error))?;
    sync_directory(root)
}

fn purge_quarantine_incidents(database: &Path) -> Result<usize, StorageError> {
    let root = database_parent(database)?.join(DATABASE_QUARANTINE_DIRECTORY);
    let incidents = validated_incident_directories(&root)?;
    let mut eligible = Vec::new();
    for incident in incidents {
        let state = read_incident_state(&incident)?;
        let created_at = match &state {
            Some(state) => state.created_at,
            None => fs::metadata(&incident)
                .map_err(|error| io_error(&incident, error))?
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .and_then(|duration| i64::try_from(duration.as_secs()).ok())
                .unwrap_or(i64::MAX),
        };
        eligible.push((created_at, incident, state));
    }
    eligible.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
    let selected = eligible;
    for (_, incident, state) in &selected {
        if let Some(mut state) = state.clone() {
            if !matches!(state.phase, IncidentPhase::Pending | IncidentPhase::Purging) {
                state.phase = IncidentPhase::Purging;
                let mut no_fault = |_| Ok(());
                write_incident_state(incident, &state, &mut no_fault)?;
            }
        }
        secure_purge_incident(&root, incident)?;
    }
    Ok(selected.len())
}

fn prepare_database_file(path: &Path) -> Result<(), StorageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            validate_regular_database_file(path, &metadata)?;
            repair_private_mode(path)?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            prepare_new_private_file(path)
        }
        Err(error) => Err(io_error(path, error)),
    }
}

fn validate_regular_database_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), StorageError> {
    if metadata.file_type().is_symlink() {
        return Err(StorageError::Symlink(path.to_path_buf()));
    }
    if !metadata.is_file() {
        return Err(StorageError::NotRegularFile(path.to_path_buf()));
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(StorageError::HardLink(path.to_path_buf()));
    }
    Ok(())
}

fn open_private_connection(path: &Path) -> Result<Connection, StorageError> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            path,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path has no parent",
            ),
        )
    })?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| io_error(parent, error))?;
    let file_name = path.file_name().ok_or_else(|| {
        io_error(
            path,
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "database path has no file name",
            ),
        )
    })?;
    let canonical_path = canonical_parent.join(file_name);
    let connection = Connection::open_with_flags(
        canonical_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    Ok(connection)
}

fn prepare_new_private_file(path: &Path) -> Result<(), StorageError> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"),
        )
    })?;
    ensure_private_dir(parent)?;
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path).map_err(|error| io_error(path, error))?;
    repair_private_mode(path)
}

fn repair_private_mode(path: &Path) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    validate_regular_database_file(path, &metadata)?;
    #[cfg(unix)]
    fs::set_permissions(path, Permissions::from_mode(0o600))
        .map_err(|error| io_error(path, error))?;
    Ok(())
}

fn database_sidecar_path(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn repair_private_database_files(database: &Path) -> Result<(), StorageError> {
    repair_private_mode(database)?;
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        let sidecar = database_sidecar_path(database, suffix);
        match fs::symlink_metadata(&sidecar) {
            Ok(metadata) => {
                validate_regular_database_file(&sidecar, &metadata)?;
                repair_private_mode(&sidecar)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&sidecar, error)),
        }
    }
    Ok(())
}

fn reject_unsafe_database_sidecars(database: &Path) -> Result<(), StorageError> {
    for suffix in DATABASE_SIDECAR_SUFFIXES {
        let sidecar = database_sidecar_path(database, suffix);
        match fs::symlink_metadata(&sidecar) {
            Ok(metadata) => validate_regular_database_file(&sidecar, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&sidecar, error)),
        }
    }
    Ok(())
}

fn configure_connection(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    connection.busy_timeout(Duration::from_millis(5_000))?;
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA mmap_size = 268435456;
         PRAGMA cache_size = -65536;",
    )?;
    Ok(())
}

fn user_version(connection: &Connection) -> Result<i64, rusqlite::Error> {
    connection.pragma_query_value(None, "user_version", |row| row.get(0))
}

fn set_schema_version(connection: &Connection, version: i64) -> Result<(), rusqlite::Error> {
    let defensive = connection.db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)?;
    if defensive {
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, false)?;
    }

    let result = connection.pragma_update(None, "schema_version", version);

    if defensive {
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    }
    result
}

fn snapshot_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Snapshot> {
    Ok(Snapshot {
        snapshot_id: row.get(offset)?,
        content: row.get(offset + 1)?,
        app: row.get(offset + 2)?,
        window_title: row.get(offset + 3)?,
        url: row.get(offset + 4)?,
        domain: row.get(offset + 5)?,
        captured_at: row.get(offset + 6)?,
        last_seen_at: row.get(offset + 7)?,
        duration_s: row.get(offset + 8)?,
        sighting_count: row.get(offset + 9)?,
        focused_name: row.get(offset + 10)?,
        focused_role: row.get(offset + 11)?,
        focused_path: row.get(offset + 12)?,
    })
}

fn chronicle_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Chronicle> {
    Ok(Chronicle {
        chronicle_id: row.get(0)?,
        level: row.get(1)?,
        period_key: row.get(2)?,
        summary_text: row.get(3)?,
        snapshot_ids: row.get(4)?,
        child_ids: row.get(5)?,
        token_count: row.get(6)?,
        generated_at: row.get(7)?,
        model_used: row.get(8)?,
        is_dirty: row.get(9)?,
    })
}

fn insert_chronicle(
    connection: &Connection,
    chronicle: &ChronicleWrite,
) -> Result<usize, rusqlite::Error> {
    connection.execute(
        "INSERT INTO chronicle
         (chronicle_id, level, period_key, summary_text, snapshot_ids, child_ids,
          token_count, generated_at, model_used, is_dirty)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)
         ON CONFLICT(level, period_key) DO NOTHING",
        params![
            chronicle.chronicle_id,
            chronicle.level,
            chronicle.period_key,
            chronicle.summary_text,
            chronicle.snapshot_ids,
            chronicle.child_ids,
            chronicle.token_count,
            chronicle.generated_at,
            chronicle.model_used,
        ],
    )
}

fn nudge_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Nudge> {
    Ok(Nudge {
        nudge_id: row.get(0)?,
        kind: row.get(1)?,
        dedupe_key: row.get(2)?,
        scheduled_for: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        deep_link: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        sent_at: row.get(9)?,
        seen_at: row.get(10)?,
        dismissed_at: row.get(11)?,
        meta_json: row.get(12)?,
    })
}

fn proactive_rule_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProactiveRule> {
    Ok(ProactiveRule {
        rule_id: row.get(0)?,
        label: row.get(1)?,
        prompt: row.get(2)?,
        schedule_kind: row.get(3)?,
        days_of_week: row.get(4)?,
        hour: row.get(5)?,
        minute: row.get(6)?,
        interval_minutes: row.get(7)?,
        timezone: row.get(8)?,
        enabled: row.get::<_, i64>(9)? != 0,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        last_fired_at: row.get(12)?,
        fire_at: row.get(13)?,
    })
}

fn time_rule_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TimeRule> {
    Ok(TimeRule {
        rule_id: row.get(0)?,
        project: row.get(1)?,
        app: row.get(2)?,
        domain: row.get(3)?,
        title_contains: row.get(4)?,
        source: row.get(5)?,
        created_at: row.get(6)?,
    })
}

fn inline_output_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InlineOutput> {
    Ok(InlineOutput {
        output_id: row.get(0)?,
        app: row.get(1)?,
        domain: row.get(2)?,
        instruction: row.get(3)?,
        output: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn wiki_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WikiPage> {
    Ok(WikiPage {
        slug: row.get(0)?,
        page_type: row.get(1)?,
        title: row.get(2)?,
        aliases: row.get(3)?,
        summary: row.get(4)?,
        body: row.get(5)?,
        links: row.get(6)?,
        snapshot_ids: row.get(7)?,
        mention_count: row.get(8)?,
        first_seen: row.get(9)?,
        last_seen: row.get(10)?,
        is_dirty: row.get(11)?,
        updated_at: row.get(12)?,
        model_used: row.get(13)?,
    })
}

fn wiki_summary_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WikiSummary> {
    Ok(WikiSummary {
        slug: row.get(0)?,
        page_type: row.get(1)?,
        title: row.get(2)?,
        summary: row.get(3)?,
        mention_count: row.get(4)?,
        last_seen: row.get(5)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, StorageError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

fn fts_literal(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn bounded_limit(requested: usize, maximum: usize) -> usize {
    requested.clamp(1, maximum)
}

fn tail_excerpt(content: &str, maximum_characters: usize) -> String {
    let mut characters = content
        .chars()
        .rev()
        .take(maximum_characters)
        .collect::<Vec<_>>();
    characters.reverse();
    characters.into_iter().collect()
}

#[cfg(test)]
mod unit_tests {
    use chrono::Timelike;

    use super::*;

    #[test]
    fn startup_recovery_is_limited_to_durable_database_failures() {
        let sqlite_error = |code| {
            StorageError::Sqlite(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ))
        };
        assert_eq!(
            recoverable_startup_reason(&sqlite_error(rusqlite::ffi::SQLITE_CORRUPT)),
            Some(StorageRecoveryReason::Corrupt)
        );
        assert_eq!(
            recoverable_startup_reason(&sqlite_error(rusqlite::ffi::SQLITE_NOTADB)),
            Some(StorageRecoveryReason::Corrupt)
        );
        for transient in [
            rusqlite::ffi::SQLITE_BUSY,
            rusqlite::ffi::SQLITE_LOCKED,
            rusqlite::ffi::SQLITE_READONLY,
            rusqlite::ffi::SQLITE_IOERR,
            rusqlite::ffi::SQLITE_CANTOPEN,
        ] {
            assert_eq!(recoverable_startup_reason(&sqlite_error(transient)), None);
        }
    }

    #[test]
    fn configured_connections_enable_defensive_mode() {
        let connection = Connection::open_in_memory().expect("open database");
        connection
            .set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, false)
            .expect("disable defensive mode for fixture");

        configure_connection(&connection).expect("configure database");

        assert!(connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
            .expect("read defensive mode"));
    }

    #[test]
    fn failed_schema_cookie_write_restores_defensive_mode() {
        let connection = Connection::open_in_memory().expect("open database");
        connection
            .set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
            .expect("enable defensive mode");
        connection
            .pragma_update(None, "query_only", true)
            .expect("make database read-only");

        assert!(set_schema_version(&connection, 92).is_err());
        assert!(connection
            .db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE)
            .expect("read defensive mode"));
    }

    fn recovery_test_directory(label: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "woof-storage-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).expect("create recovery fixture directory");
        directory
    }

    fn incident_contains_bytes(database: &Path, expected: &[u8]) -> bool {
        let root = database
            .parent()
            .expect("database parent")
            .join(DATABASE_QUARANTINE_DIRECTORY);
        validated_incident_directories(&root)
            .expect("validated incidents")
            .into_iter()
            .map(|incident| incident_file_path(&incident, ""))
            .filter(|path| path.exists())
            .any(|path| fs::read(path).expect("read quarantined database") == expected)
    }

    #[test]
    fn every_recovery_operation_is_restart_safe() {
        let original = b"not a sqlite database; crash fixture";
        let mut fault_index = 0_usize;
        loop {
            let directory = recovery_test_directory("fault-injection");
            let database = directory.join("woof.db");
            fs::write(&database, original).expect("malformed fixture");
            let mut operation_index = 0_usize;
            let mut observed = Vec::new();
            let result = Storage::open_or_recover_with(&database, &mut |operation| {
                observed.push(operation);
                let current = operation_index;
                operation_index += 1;
                if current == fault_index {
                    Err(StorageError::RecoveryInterrupted)
                } else {
                    Ok(())
                }
            });

            if result.is_ok() {
                assert_eq!(fault_index, operation_index);
                assert_eq!(fault_index, 26, "every durable operation is injectable");
                assert_eq!(
                    observed,
                    vec![
                        RecoveryOperation::QuarantineRootCreated,
                        RecoveryOperation::QuarantineRootParentSynced,
                        RecoveryOperation::IncidentDirectoryCreated,
                        RecoveryOperation::IncidentDirectorySynced,
                        RecoveryOperation::StateTempCreated,
                        RecoveryOperation::StateTempWritten,
                        RecoveryOperation::StateTempSynced,
                        RecoveryOperation::StateRenamed,
                        RecoveryOperation::StateDirectorySynced,
                        RecoveryOperation::CopyCreated(0),
                        RecoveryOperation::CopyWritten(0),
                        RecoveryOperation::CopySynced(0),
                        RecoveryOperation::CopiesDirectorySynced,
                        RecoveryOperation::SourcesRevalidated,
                        RecoveryOperation::StateTempCreated,
                        RecoveryOperation::StateTempWritten,
                        RecoveryOperation::StateTempSynced,
                        RecoveryOperation::StateRenamed,
                        RecoveryOperation::StateDirectorySynced,
                        RecoveryOperation::OriginalRemoved(0),
                        RecoveryOperation::SourceDirectorySynced,
                        RecoveryOperation::StateTempCreated,
                        RecoveryOperation::StateTempWritten,
                        RecoveryOperation::StateTempSynced,
                        RecoveryOperation::StateRenamed,
                        RecoveryOperation::StateDirectorySynced,
                    ]
                );
                fs::remove_dir_all(directory).expect("remove fixture directory");
                break;
            }
            assert!(matches!(result, Err(StorageError::RecoveryInterrupted)));

            let startup = Storage::open_or_recover(&database).expect("reconcile after crash");
            assert!(incident_contains_bytes(&database, original));
            assert_eq!(
                startup
                    .storage
                    .connect()
                    .expect("fresh database")
                    .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                    .expect("schema version"),
                SCHEMA_VERSION
            );
            fs::remove_dir_all(directory).expect("remove fixture directory");
            fault_index += 1;
            assert!(fault_index < 100, "recovery operation count is bounded");
        }
    }

    #[test]
    fn live_wal_is_preserved_with_the_database_family() {
        let directory = recovery_test_directory("live-wal");
        let database = directory.join("woof.db");
        let connection = Connection::open(&database).expect("open fixture database");
        connection
            .execute_batch(
                "PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE fixture(value TEXT);
                 PRAGMA user_version = 17;
                 INSERT INTO fixture VALUES ('row committed in live wal');",
            )
            .expect("create live WAL fixture");
        assert!(database_sidecar_path(&database, "-wal").exists());

        let startup = Storage::open_or_recover(&database).expect("recover live WAL database");
        let quarantined = startup
            .recovery
            .expect("recovery report")
            .quarantined_database_path;
        drop(connection);
        let quarantined = Connection::open(quarantined).expect("open quarantined database");
        assert_eq!(
            quarantined
                .query_row("SELECT value FROM fixture", [], |row| row
                    .get::<_, String>(0))
                .expect("read live WAL row"),
            "row committed in live wal"
        );
        drop(quarantined);
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[test]
    fn every_present_database_sidecar_is_copied_before_source_removal() {
        let directory = recovery_test_directory("all-sidecars");
        let database = directory.join("woof.db");
        fs::write(&database, b"not a sqlite database; family fixture").expect("malformed fixture");
        for (suffix, bytes) in [
            ("-wal", b"wal-private".as_slice()),
            ("-shm", b"shm-private".as_slice()),
            ("-journal", b"journal-private".as_slice()),
        ] {
            fs::write(database_sidecar_path(&database, suffix), bytes).expect("sidecar fixture");
        }

        let expected = database_file_identity(&database)
            .expect("database identity")
            .expect("database fixture");
        let quarantined = quarantine_database_family(
            &database,
            &expected,
            StorageRecoveryReason::Corrupt,
            &mut |_| Ok(()),
        )
        .expect("quarantine database family");
        let incident = quarantined.parent().expect("incident").to_path_buf();
        for (suffix, bytes) in [
            ("-wal", b"wal-private".as_slice()),
            ("-shm", b"shm-private".as_slice()),
            ("-journal", b"journal-private".as_slice()),
        ] {
            assert_eq!(
                fs::read(incident_file_path(&incident, suffix)).expect("quarantined sidecar"),
                bytes
            );
        }
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    #[test]
    fn pending_incident_is_cleaned_before_retrying_source_recovery() {
        let directory = recovery_test_directory("pending-reconcile");
        let database = directory.join("woof.db");
        let original = b"not a sqlite database; pending fixture";
        fs::write(&database, original).expect("malformed fixture");
        let result = Storage::open_or_recover_with(&database, &mut |operation| {
            if operation == RecoveryOperation::SourcesRevalidated {
                Err(StorageError::RecoveryInterrupted)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(StorageError::RecoveryInterrupted)));
        assert_eq!(fs::read(&database).expect("source remains"), original);

        Storage::open_or_recover(&database).expect("retry recovery");
        assert!(incident_contains_bytes(&database, original));
        let root = directory.join(DATABASE_QUARANTINE_DIRECTORY);
        assert_eq!(
            validated_incident_directories(&root)
                .expect("incidents")
                .len(),
            1,
            "partial pre-ready incident is securely removed"
        );
        fs::remove_dir_all(directory).expect("remove fixture directory");
    }

    fn rule(schedule_kind: &str) -> ProactiveRule {
        ProactiveRule {
            rule_id: Some("fixture".to_string()),
            label: "Fixture".to_string(),
            prompt: "Fixture".to_string(),
            schedule_kind: schedule_kind.to_string(),
            days_of_week: String::new(),
            hour: 9,
            minute: 0,
            interval_minutes: 0,
            timezone: "local".to_string(),
            enabled: true,
            created_at: 1_000,
            updated_at: 1_000,
            last_fired_at: None,
            fire_at: None,
        }
    }

    #[test]
    fn due_rule_slots_cover_only_canonical_schedules() {
        let mut one_shot = rule("once");
        one_shot.fire_at = Some(2_000);
        assert_eq!(due_rule_slot(&one_shot, 1_999), None);
        assert_eq!(due_rule_slot(&one_shot, 2_000), Some(2_000));
        one_shot.last_fired_at = Some(2_000);
        assert_eq!(due_rule_slot(&one_shot, 3_000), None);

        let mut interval = rule("interval");
        interval.interval_minutes = 5;
        assert_eq!(due_rule_slot(&interval, 1_299), None);
        assert_eq!(due_rule_slot(&interval, 1_300), Some(1_300));
        interval.last_fired_at = Some(1_300);
        assert_eq!(due_rule_slot(&interval, 1_899), Some(1_600));

        let local_now = Local::now();
        let mut daily = rule("daily");
        daily.hour = i64::from(local_now.hour());
        daily.minute = i64::from(local_now.minute());
        let expected_slot = Local
            .from_local_datetime(
                &local_now
                    .date_naive()
                    .and_hms_opt(local_now.hour(), local_now.minute(), 0)
                    .expect("valid minute"),
            )
            .earliest()
            .expect("local minute")
            .timestamp();
        assert_eq!(
            due_rule_slot(&daily, local_now.timestamp()),
            Some(expected_slot)
        );
        daily.last_fired_at = Some(expected_slot);
        assert_eq!(due_rule_slot(&daily, local_now.timestamp()), None);

        let mut weekly = rule("weekly");
        weekly.hour = i64::from(local_now.hour());
        weekly.minute = i64::from(local_now.minute());
        weekly.days_of_week = (local_now.weekday().num_days_from_monday() + 1).to_string();
        assert_eq!(
            due_rule_slot(&weekly, local_now.timestamp()),
            Some(expected_slot)
        );
        weekly.days_of_week = local_now.weekday().to_string();
        assert_eq!(due_rule_slot(&weekly, local_now.timestamp()), None);
        weekly.schedule_kind = "unsupported".to_string();
        weekly.fire_at = Some(local_now.timestamp());
        assert_eq!(due_rule_slot(&weekly, local_now.timestamp()), None);
    }

    #[test]
    fn fractional_offset_hour_buckets_start_on_local_hours() {
        let india_offset = 5 * 3_600 + 30 * 60;
        assert_eq!(hour_bucket_for_offset(0, india_offset), -1_800);
        assert_eq!(hour_bucket_for_offset(1_800, india_offset), 1_800);

        let local_midnight = -india_offset;
        let quarter_past_midnight = local_midnight + 15 * 60;
        assert_eq!(
            hour_bucket_for_offset(quarter_past_midnight, india_offset),
            local_midnight
        );
    }
}
