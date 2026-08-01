use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::{symlink, PermissionsExt};

use rusqlite::Connection;
use uuid::Uuid;
use woof_storage::{
    Storage, StorageError, StorageRecoveryReason, LOGICAL_TABLES, NAMED_INDEXES, SCHEMA_SQL,
    SCHEMA_VERSION,
};

fn temporary_directory(label: &str) -> PathBuf {
    let unique = Uuid::new_v4().simple();
    let path = std::env::temp_dir().join(format!(
        "woof-storage-{label}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn names(connection: &Connection, object_type: &str, predicate: &str) -> BTreeSet<String> {
    let sql =
        format!("SELECT name FROM sqlite_master WHERE type = ?1 AND {predicate} ORDER BY name");
    connection
        .prepare(&sql)
        .expect("prepare catalog query")
        .query_map([object_type], |row| row.get::<_, String>(0))
        .expect("query catalog")
        .collect::<Result<_, _>>()
        .expect("collect catalog")
}

fn remove_directory(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

fn sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn tree_contains_bytes(path: &Path, needle: &[u8]) -> bool {
    let metadata = fs::symlink_metadata(path).expect("tree metadata");
    if metadata.is_file() {
        return fs::read(path)
            .expect("read tree file")
            .windows(needle.len())
            .any(|window| window == needle);
    }
    if !metadata.is_dir() {
        return false;
    }
    fs::read_dir(path)
        .expect("read tree directory")
        .map(|entry| entry.expect("tree entry").path())
        .any(|entry| tree_contains_bytes(&entry, needle))
}

#[test]
fn fresh_database_matches_the_v18_catalog_contract() {
    let directory = temporary_directory("schema");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    let connection = storage.connect().expect("connect");

    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("user_version");
    let schema_version: i64 = connection
        .pragma_query_value(None, "schema_version", |row| row.get(0))
        .expect("schema_version");
    assert_eq!(user_version, SCHEMA_VERSION);
    assert_eq!(schema_version, 92);

    let logical_tables = names(
        &connection,
        "table",
        "name NOT LIKE 'sqlite_%'
         AND name NOT LIKE '%_fts'
         AND name NOT LIKE '%_fts_%'",
    );
    assert_eq!(
        logical_tables,
        LOGICAL_TABLES
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        names(&connection, "index", "name NOT LIKE 'sqlite_%'"),
        NAMED_INDEXES
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        names(
            &connection,
            "table",
            "name IN ('snapshots_fts','index_fts','wiki_fts')"
        ),
        ["index_fts", "snapshots_fts", "wiki_fts"]
            .into_iter()
            .map(ToString::to_string)
            .collect()
    );

    let triggers = names(&connection, "trigger", "1 = 1");
    assert_eq!(
        triggers,
        [
            "index_entries_ad",
            "index_entries_ai",
            "index_entries_au",
            "snapshots_ai",
            "snapshots_au",
            "wiki_pages_ad",
            "wiki_pages_ai",
            "wiki_pages_au",
        ]
        .into_iter()
        .map(ToString::to_string)
        .collect()
    );
    assert!(!triggers.contains("snapshots_ad"));

    // The non-STRICT text primary key deliberately retains SQLite's NULL behavior.
    connection
        .execute(
            "INSERT INTO chronicle
             (chronicle_id, level, period_key, summary_text, generated_at)
             VALUES (NULL, 'day', 'fixture', 'synthetic', 1)",
            [],
        )
        .expect("nullable text primary key");
    let repair_connection = storage.connect().expect("repair database file modes");

    #[cfg(unix)]
    for path in [
        database.clone(),
        sidecar(&database, "-wal"),
        sidecar(&database, "-shm"),
    ] {
        if path.exists() {
            assert_eq!(
                fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
                0o600
            );
        }
    }
    drop(repair_connection);
    drop(connection);
    remove_directory(&directory);
}

#[test]
fn populated_databases_must_already_be_version_18() {
    let directory = temporary_directory("unsupported-version");
    let database = directory.join("woof.db");
    let connection = Connection::open(&database).expect("fixture database");
    connection
        .execute_batch("CREATE TABLE fixture(value TEXT); PRAGMA user_version = 17;")
        .expect("fixture schema");
    drop(connection);

    assert!(matches!(
        Storage::open(&database),
        Err(StorageError::UnsupportedVersion {
            found: 17,
            required: SCHEMA_VERSION
        })
    ));
    remove_directory(&directory);
}

#[test]
fn valid_version_18_data_is_never_displaced_during_recovery_open() {
    let directory = temporary_directory("valid-recovery-open");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    storage
        .connect()
        .expect("connect")
        .execute(
            "INSERT INTO snapshots (
                snapshot_id, content, app, window_title, captured_at,
                last_seen_at, duration_s, sighting_count
             ) VALUES ('kept', 'private fixture', 'TextEdit', 'Draft', 1, 1, 0, 1)",
            [],
        )
        .expect("insert fixture");

    let startup = Storage::open_or_recover(&database).expect("open valid database");
    assert!(startup.recovery.is_none());
    let content: String = startup
        .storage
        .connect()
        .expect("connect after open")
        .query_row(
            "SELECT content FROM snapshots WHERE snapshot_id = 'kept'",
            [],
            |row| row.get(0),
        )
        .expect("retained content");
    assert_eq!(content, "private fixture");
    assert!(!directory.join("database-quarantine").exists());
    remove_directory(&directory);
}

#[test]
fn unsupported_database_is_quarantined_with_its_data_before_fresh_v18_creation() {
    let directory = temporary_directory("recover-unsupported");
    let database = directory.join("woof.db");
    let connection = Connection::open(&database).expect("fixture database");
    connection
        .execute_batch(
            "CREATE TABLE fixture(value TEXT);
             INSERT INTO fixture VALUES ('retained private row');
             PRAGMA user_version = 17;",
        )
        .expect("fixture schema");
    drop(connection);

    let startup = Storage::open_or_recover(&database).expect("recover database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::UnsupportedVersion);
    assert!(recovery.quarantined_database_path.exists());
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    let retained: String = quarantined
        .query_row("SELECT value FROM fixture", [], |row| row.get(0))
        .expect("retained row");
    assert_eq!(retained, "retained private row");
    drop(quarantined);

    assert_eq!(
        startup
            .storage
            .connect()
            .expect("fresh connection")
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("fresh version"),
        SCHEMA_VERSION
    );

    #[cfg(unix)]
    {
        let incident = recovery
            .quarantined_database_path
            .parent()
            .expect("incident directory");
        let quarantine_root = incident.parent().expect("quarantine root");
        assert_eq!(
            fs::metadata(&recovery.quarantined_database_path)
                .expect("quarantined mode")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(incident)
                .expect("incident mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(quarantine_root)
                .expect("quarantine root mode")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for entry in fs::read_dir(incident).expect("incident files") {
            let path = entry.expect("incident entry").path();
            assert_eq!(
                fs::metadata(&path)
                    .expect("incident file mode")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "{} must remain private",
                path.display()
            );
        }
    }
    remove_directory(&directory);
}

#[test]
fn malformed_database_bytes_are_preserved_exactly_before_reinitialization() {
    let directory = temporary_directory("recover-malformed");
    let database = directory.join("woof.db");
    let original = b"not a sqlite database; private fixture";
    fs::write(&database, original).expect("malformed fixture");

    let startup = Storage::open_or_recover(&database).expect("recover malformed database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::Corrupt);
    assert_eq!(
        fs::read(&recovery.quarantined_database_path).expect("quarantined bytes"),
        original
    );
    assert_eq!(
        startup
            .storage
            .connect()
            .expect("fresh connection")
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("fresh version"),
        SCHEMA_VERSION
    );
    remove_directory(&directory);
}

#[test]
fn incompatible_version_18_schema_is_preserved_instead_of_restart_looping() {
    let directory = temporary_directory("recover-incompatible-schema");
    let database = directory.join("woof.db");
    let connection = Connection::open(&database).expect("fixture database");
    connection
        .execute_batch(
            "CREATE TABLE fixture(value TEXT);
             INSERT INTO fixture VALUES ('preserved');
             PRAGMA user_version = 18;",
        )
        .expect("fixture schema");
    drop(connection);

    let startup = Storage::open_or_recover(&database).expect("recover incompatible schema");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::IncompatibleSchema);
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    assert_eq!(
        quarantined
            .query_row("SELECT value FROM fixture", [], |row| row
                .get::<_, String>(0))
            .expect("preserved fixture"),
        "preserved"
    );
    remove_directory(&directory);
}

#[test]
fn replaced_named_trigger_body_is_quarantined_as_incompatible() {
    let directory = temporary_directory("replaced-trigger");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    storage
        .connect()
        .expect("connect")
        .execute_batch(
            "DROP TRIGGER snapshots_ai;
             CREATE TRIGGER snapshots_ai AFTER INSERT ON snapshots BEGIN SELECT 1; END;",
        )
        .expect("replace trigger body");
    drop(storage);

    let startup = Storage::open_or_recover(&database).expect("replace incompatible database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::IncompatibleSchema);
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    let sql: String = quarantined
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'snapshots_ai'",
            [],
            |row| row.get(0),
        )
        .expect("replaced trigger SQL");
    assert!(sql.contains("SELECT 1"));
    drop(quarantined);
    remove_directory(&directory);
}

#[test]
fn replaced_named_unique_partial_index_is_quarantined_as_incompatible() {
    let directory = temporary_directory("replaced-index");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    storage
        .connect()
        .expect("connect")
        .execute_batch(
            "DROP INDEX idx_nudges_dedupe;
             CREATE INDEX idx_nudges_dedupe ON nudges(dedupe_key);",
        )
        .expect("replace index semantics");
    drop(storage);

    let startup = Storage::open_or_recover(&database).expect("replace incompatible database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::IncompatibleSchema);
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    let sql: String = quarantined
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_nudges_dedupe'",
            [],
            |row| row.get(0),
        )
        .expect("replaced index SQL");
    assert!(!sql.to_ascii_uppercase().contains("UNIQUE"));
    assert!(!sql.to_ascii_uppercase().contains("WHERE"));
    drop(quarantined);
    remove_directory(&directory);
}

#[test]
fn quoted_constant_expression_index_cannot_mimic_the_canonical_sql() {
    let directory = temporary_directory("quoted-expression-index");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    storage
        .connect()
        .expect("connect")
        .execute_batch(
            "DROP INDEX idx_wf_name_lower;
             CREATE UNIQUE INDEX idx_wf_name_lower ON workflows(\"lower(name)\");",
        )
        .expect("replace index with quoted constant expression");
    drop(storage);

    let startup = Storage::open_or_recover(&database).expect("replace incompatible database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::IncompatibleSchema);
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    let sql: String = quarantined
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_wf_name_lower'",
            [],
            |row| row.get(0),
        )
        .expect("quoted index SQL");
    assert!(sql.contains("\"lower(name)\""));
    drop(quarantined);
    remove_directory(&directory);
}

#[test]
fn quoted_trigger_reference_cannot_mimic_the_canonical_sql() {
    let directory = temporary_directory("quoted-trigger-reference");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    storage
        .connect()
        .expect("connect")
        .execute_batch(
            "DROP TRIGGER snapshots_ai;
             CREATE TRIGGER snapshots_ai AFTER INSERT ON snapshots BEGIN
               INSERT INTO snapshots_fts(rowid, content, app, window_title, domain)
               VALUES (\"new.rowid\", new.content, new.app, new.window_title, new.domain);
             END;",
        )
        .expect("replace trigger with quoted reference");
    drop(storage);

    let startup = Storage::open_or_recover(&database).expect("replace incompatible database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::IncompatibleSchema);
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    let sql: String = quarantined
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'snapshots_ai'",
            [],
            |row| row.get(0),
        )
        .expect("quoted trigger SQL");
    assert!(sql.contains("\"new.rowid\""));
    drop(quarantined);
    remove_directory(&directory);
}

#[test]
fn extra_table_unique_constraint_is_quarantined_as_incompatible() {
    let directory = temporary_directory("extra-table-unique");
    let database = directory.join("woof.db");
    let modified_schema = SCHEMA_SQL.replacen(
        "content        TEXT NOT NULL,",
        "content        TEXT NOT NULL UNIQUE,",
        1,
    );
    assert_ne!(modified_schema, SCHEMA_SQL);
    let connection = Connection::open(&database).expect("open modified fixture database");
    connection
        .execute_batch(&modified_schema)
        .expect("create modified schema");
    drop(connection);

    let startup = Storage::open_or_recover(&database).expect("replace incompatible database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::IncompatibleSchema);
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    let sql: String = quarantined
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'snapshots'",
            [],
            |row| row.get(0),
        )
        .expect("modified table SQL");
    assert!(sql.contains("TEXT NOT NULL UNIQUE"));
    drop(quarantined);
    remove_directory(&directory);
}

#[test]
fn altered_virtual_table_options_are_quarantined_as_incompatible() {
    let directory = temporary_directory("altered-virtual-table");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    storage
        .connect()
        .expect("connect")
        .execute_batch(
            "DROP TABLE snapshots_fts;
             CREATE VIRTUAL TABLE snapshots_fts USING fts5(
               content, app, window_title, domain
             );",
        )
        .expect("replace virtual table options");
    drop(storage);

    let startup = Storage::open_or_recover(&database).expect("replace incompatible database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::IncompatibleSchema);
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    let sql: String = quarantined
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'snapshots_fts'",
            [],
            |row| row.get(0),
        )
        .expect("modified virtual table SQL");
    assert!(!sql.contains("content='snapshots'"));
    drop(quarantined);
    remove_directory(&directory);
}

#[test]
fn extra_fts_suffixed_table_cannot_escape_the_exact_catalog_contract() {
    let directory = temporary_directory("extra-fts-table");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    storage
        .connect()
        .expect("connect")
        .execute_batch(
            "CREATE TABLE hidden_fts(secret TEXT);
             INSERT INTO hidden_fts VALUES ('must be quarantined');",
        )
        .expect("create hidden extra table");
    drop(storage);

    let startup = Storage::open_or_recover(&database).expect("replace incompatible database");
    let recovery = startup.recovery.expect("recovery report");
    assert_eq!(recovery.reason, StorageRecoveryReason::IncompatibleSchema);
    let quarantined =
        Connection::open(&recovery.quarantined_database_path).expect("open quarantined database");
    let secret: String = quarantined
        .query_row("SELECT secret FROM hidden_fts", [], |row| row.get(0))
        .expect("preserved hidden table data");
    assert_eq!(secret, "must be quarantined");
    drop(quarantined);
    remove_directory(&directory);
}

#[cfg(unix)]
#[test]
fn database_and_sidecar_symlinks_are_rejected() {
    let directory = temporary_directory("symlinks");
    let outside = directory.join("outside");
    fs::write(&outside, b"private").expect("outside fixture");

    let database_link = directory.join("linked.db");
    symlink(&outside, &database_link).expect("database symlink");
    assert!(matches!(
        Storage::open(&database_link),
        Err(StorageError::Symlink(path)) if path == database_link
    ));
    assert!(matches!(
        Storage::open_or_recover(&database_link),
        Err(StorageError::Symlink(path)) if path == database_link
    ));

    let database = directory.join("woof.db");
    fs::write(&database, []).expect("database fixture");
    let wal = sidecar(&database, "-wal");
    symlink(&outside, &wal).expect("wal symlink");
    assert!(matches!(
        Storage::open(&database),
        Err(StorageError::Symlink(path)) if path == wal
    ));
    fs::remove_file(&wal).expect("remove wal fixture");
    let journal = sidecar(&database, "-journal");
    symlink(&outside, &journal).expect("journal symlink");
    assert!(matches!(
        Storage::open_or_recover(&database),
        Err(StorageError::Symlink(path)) if path == journal
    ));
    assert_eq!(fs::read(&outside).expect("outside unchanged"), b"private");
    assert!(!directory.join("database-quarantine").exists());
    remove_directory(&directory);
}

#[cfg(unix)]
#[test]
fn unsafe_quarantine_directory_cannot_displace_a_recovery_candidate() {
    let directory = temporary_directory("unsafe-quarantine");
    let database = directory.join("woof.db");
    let original = b"not a sqlite database";
    fs::write(&database, original).expect("malformed fixture");
    let outside = directory.join("outside");
    fs::create_dir(&outside).expect("outside directory");
    let quarantine = directory.join("database-quarantine");
    symlink(&outside, &quarantine).expect("quarantine symlink");

    assert!(Storage::open_or_recover(&database).is_err());
    assert_eq!(fs::read(&database).expect("candidate unchanged"), original);
    assert!(fs::read_dir(&outside)
        .expect("outside contents")
        .next()
        .is_none());
    remove_directory(&directory);
}

#[test]
fn delete_all_data_preserves_v18_and_clears_external_fts_and_sequences() {
    let directory = temporary_directory("delete-all");
    let database = directory.join("woof.db");
    let storage = Storage::open(&database).expect("create database");
    let connection = storage.connect().expect("connect");
    connection
        .execute_batch(
            "INSERT INTO snapshots (
                snapshot_id, content, app, window_title, captured_at,
                last_seen_at, duration_s, sighting_count
             ) VALUES ('snapshot-delete', 'private phrase', 'TextEdit', 'Draft', 1, 1, 0, 1);
             INSERT INTO index_entries (topic, entities, domain, snapshot_ids, created_at)
             VALUES ('private topic', '[]', '', '[\"snapshot-delete\"]', 1);
             INSERT INTO wiki_pages (
                slug, page_type, title, aliases, summary, body, links,
                snapshot_ids, mention_count, first_seen, last_seen,
                is_dirty, updated_at
             ) VALUES (
                'private-page', 'project', 'Private Page', '[]', 'private',
                'private body', '[]', '[\"snapshot-delete\"]', 1, 1, 1, 0, 1
             );
             INSERT INTO style_notes (source, surface, bullet, updated_at)
             VALUES ('test', 'test', 'private style', 1);",
        )
        .expect("insert fixtures");
    drop(connection);

    assert_eq!(storage.delete_all_data().expect("delete all data"), 4);

    let connection = storage.connect().expect("reconnect");
    for table in LOGICAL_TABLES {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("logical table count");
        assert_eq!(count, 0, "{table} should be empty");
    }
    for table in ["snapshots_fts", "index_fts", "wiki_fts"] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("fts count");
        assert_eq!(count, 0, "{table} should be empty");
    }
    let sequence_count: i64 = connection
        .query_row("SELECT count(*) FROM sqlite_sequence", [], |row| row.get(0))
        .expect("sequence count");
    assert_eq!(sequence_count, 0);
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("user version"),
        SCHEMA_VERSION
    );
    drop(connection);
    remove_directory(&directory);
}

#[test]
fn delete_all_securely_purges_quarantined_and_current_secrets() {
    let directory = temporary_directory("delete-all-quarantine");
    let database = directory.join("woof.db");
    let quarantined_secret = b"quarantine-secret-6cf752d4";
    fs::write(&database, quarantined_secret).expect("malformed private fixture");
    let startup = Storage::open_or_recover(&database).expect("recover fixture");
    let recovery = startup.recovery.as_ref().expect("recovery report");
    assert!(tree_contains_bytes(
        recovery
            .quarantined_database_path
            .parent()
            .expect("incident"),
        quarantined_secret
    ));
    let current_secret = b"current-secret-e8c49f13";
    startup
        .storage
        .connect()
        .expect("current database")
        .execute(
            "INSERT INTO snapshots (
                snapshot_id, content, app, window_title, captured_at,
                last_seen_at, duration_s, sighting_count
             ) VALUES ('delete-secret', ?1, 'TextEdit', 'Draft', 1, 1, 0, 1)",
            [std::str::from_utf8(current_secret).expect("UTF-8 fixture")],
        )
        .expect("insert current secret");

    assert_eq!(startup.storage.delete_all_data().expect("delete all"), 1);
    let quarantine_root = directory.join("database-quarantine");
    assert_eq!(
        fs::read_dir(&quarantine_root)
            .expect("quarantine root")
            .count(),
        0
    );
    assert!(!tree_contains_bytes(&directory, quarantined_secret));
    assert!(!tree_contains_bytes(&directory, current_secret));
    remove_directory(&directory);
}

#[cfg(unix)]
#[test]
fn delete_all_rejects_unexpected_quarantine_symlink_before_deleting_data() {
    let directory = temporary_directory("delete-all-unsafe-quarantine");
    let database = directory.join("woof.db");
    fs::write(&database, b"malformed private fixture").expect("malformed fixture");
    let startup = Storage::open_or_recover(&database).expect("recover fixture");
    startup
        .storage
        .connect()
        .expect("current database")
        .execute(
            "INSERT INTO snapshots (
                snapshot_id, content, app, window_title, captured_at,
                last_seen_at, duration_s, sighting_count
             ) VALUES ('must-remain', 'private', 'TextEdit', 'Draft', 1, 1, 0, 1)",
            [],
        )
        .expect("insert fixture");
    let outside = directory.join("outside-secret");
    fs::write(&outside, b"outside").expect("outside fixture");
    let incident = startup
        .recovery
        .as_ref()
        .expect("recovery")
        .quarantined_database_path
        .parent()
        .expect("incident");
    symlink(&outside, incident.join(".state.tmp")).expect("unsafe state temp");

    assert!(startup.storage.delete_all_data().is_err());
    assert_eq!(fs::read(&outside).expect("outside unchanged"), b"outside");
    let retained: i64 = startup
        .storage
        .connect()
        .expect("reconnect")
        .query_row(
            "SELECT count(*) FROM snapshots WHERE snapshot_id = 'must-remain'",
            [],
            |row| row.get(0),
        )
        .expect("retained current data");
    assert_eq!(retained, 1);
    remove_directory(&directory);
}

#[cfg(unix)]
#[test]
fn delete_all_rejects_quarantine_hard_links_without_touching_the_other_inode_name() {
    let directory = temporary_directory("delete-all-hard-link");
    let database = directory.join("woof.db");
    fs::write(&database, b"malformed private fixture").expect("malformed fixture");
    let startup = Storage::open_or_recover(&database).expect("recover fixture");
    startup
        .storage
        .connect()
        .expect("current database")
        .execute(
            "INSERT INTO snapshots (
                snapshot_id, content, app, window_title, captured_at,
                last_seen_at, duration_s, sighting_count
             ) VALUES ('must-remain-hard-link', 'private', 'TextEdit', 'Draft', 1, 1, 0, 1)",
            [],
        )
        .expect("insert fixture");
    let outside = directory.join("outside-hard-linked-secret");
    fs::write(&outside, b"outside-hard-linked-value").expect("outside fixture");
    let incident = startup
        .recovery
        .as_ref()
        .expect("recovery")
        .quarantined_database_path
        .parent()
        .expect("incident");
    fs::hard_link(&outside, incident.join(".state.tmp")).expect("hard-link fixture");

    assert!(matches!(
        startup.storage.delete_all_data(),
        Err(StorageError::HardLink(_))
    ));
    assert_eq!(
        fs::read(&outside).expect("outside unchanged"),
        b"outside-hard-linked-value"
    );
    let retained: i64 = startup
        .storage
        .connect()
        .expect("reconnect")
        .query_row(
            "SELECT count(*) FROM snapshots WHERE snapshot_id = 'must-remain-hard-link'",
            [],
            |row| row.get(0),
        )
        .expect("retained current data");
    assert_eq!(retained, 1);
    remove_directory(&directory);
}

#[test]
fn finite_retention_purges_every_quarantine_incident_before_returning() {
    let directory = temporary_directory("retention-quarantine");
    let database = directory.join("woof.db");
    let mut storage = None;
    for sequence in 0..33 {
        drop(storage.take());
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let path = if suffix.is_empty() {
                database.clone()
            } else {
                sidecar(&database, suffix)
            };
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("remove fixture file: {error}"),
            }
        }
        fs::write(
            &database,
            format!("malformed private fixture {sequence}").as_bytes(),
        )
        .expect("malformed fixture");
        storage = Some(
            Storage::open_or_recover(&database)
                .expect("recover fixture")
                .storage,
        );
    }
    let storage = storage.expect("latest storage");
    let root = directory.join("database-quarantine");
    assert_eq!(fs::read_dir(&root).expect("incidents").count(), 33);

    storage
        .prune_expired_data(0)
        .expect("finite retention securely removes every quarantine copy");
    assert_eq!(fs::read_dir(&root).expect("incidents").count(), 0);
    remove_directory(&directory);
}
