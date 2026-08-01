use std::{fs, path::PathBuf};

use rusqlite::Connection;
use uuid::Uuid;
use woof_storage::{Storage, SCHEMA_VERSION};

fn fixture() -> (Storage, PathBuf) {
    let unique = Uuid::new_v4().simple();
    let directory =
        std::env::temp_dir().join(format!("woof-retention-{}-{unique}", std::process::id()));
    fs::create_dir_all(&directory).expect("fixture directory");
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    (storage, directory)
}

fn count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("row count")
}

#[test]
fn pruning_removes_expired_capture_and_invalidates_generated_memory() {
    let (storage, directory) = fixture();
    let connection = storage.connect().expect("connection");
    connection
        .execute_batch(
            "INSERT INTO snapshots
                 (snapshot_id, content, app, window_title, captured_at, last_seen_at)
             VALUES
                 ('expired', 'expired private phrase', 'TextEdit', 'Old', 50, 100),
                 ('current', 'fresh retained phrase', 'TextEdit', 'New', 250, 300);

             INSERT INTO activity_events
                 (snapshot_id, app, window_title, started_at, last_seen_at)
             VALUES
                 ('expired', 'TextEdit', 'Old', 50, 100),
                 ('current', 'TextEdit', 'New', 250, 300);

             INSERT INTO working_memory (snapshot_id, added_at, relevance)
             VALUES ('expired', 100, 1), ('current', 300, 1);

             INSERT INTO chronicle
                 (chronicle_id, level, period_key, summary_text, generated_at)
             VALUES ('summary', 'day', 'fixture', 'mixed private memory', 300);

             INSERT INTO index_entries
                 (topic, entities, domain, snapshot_ids, created_at, last_updated_at)
             VALUES ('mixed private topic', '[]', '', '[\"expired\"]', 300, 300);

             INSERT INTO kg_entities (entity_id, name, entity_type, first_seen, last_seen)
             VALUES ('old-entity', 'Old Entity', 'topic', 50, 300),
                    ('new-entity', 'New Entity', 'topic', 250, 300);
             INSERT INTO kg_relations
                 (relation_id, subject_id, predicate, object_id, valid_from, source_snapshot_id)
             VALUES ('relation', 'old-entity', 'related', 'new-entity', 100, 'expired');

             INSERT INTO workflows
                 (workflow_id, name, first_detected_at, last_detected_at, generated_at)
             VALUES ('workflow', 'Mixed private workflow', 50, 300, 300);

             INSERT INTO wiki_pages
                 (slug, page_type, title, first_seen, last_seen, updated_at)
             VALUES ('mixed', 'topic', 'Mixed Private Page', 50, 300, 300);

             INSERT INTO salient_flags
                 (flag_id, kind, text, snapshot_id, period_key, created_at)
             VALUES (1, 'followup', 'old flag', 'expired', 'old', 100),
                    (2, 'followup', 'new flag', 'current', 'new', 300);

             INSERT INTO nudges
                 (nudge_id, kind, scheduled_for, title, body, status, created_at)
             VALUES ('context', 'contextual_nudge', 400, 'Mixed', 'private', 'pending', 300),
                    ('reminder', 'scheduled_rule', 400, 'User', 'reminder', 'pending', 300);

             INSERT INTO proactive_rules
                 (rule_id, label, prompt, schedule_kind, timezone, created_at, updated_at)
             VALUES ('rule', 'User rule', 'Remember this', 'once', 'local', 100, 100);

             INSERT INTO recording_events (session_started_at, ts_ms, kind)
             VALUES (100000, 100000, 'old'), (300000, 300000, 'new');
             INSERT INTO inline_rewrite_uses (app, domain, first_used_at, last_used_at)
             VALUES ('OldApp', '', 100, 100), ('NewApp', '', 300, 300);
             INSERT INTO chat_turns (role, content, created_at)
             VALUES ('user', 'old chat', 100), ('user', 'new chat', 300);
             INSERT INTO inline_outputs (output, created_at)
             VALUES ('old output', 100), ('new output', 300);
             INSERT INTO time_ledger (hour_ts, app, seconds)
             VALUES (100, 'OldApp', 1), (300, 'NewApp', 1);
             INSERT INTO time_rules (project, source, created_at)
             VALUES ('Manual', 'user', 100), ('Detected', 'capture', 300);
             INSERT INTO style_notes (source, surface, bullet, updated_at)
             VALUES ('old', 'test', 'old style', 100), ('new', 'test', 'new style', 300);",
        )
        .expect("seed retention fixtures");
    drop(connection);

    let report = storage.prune_expired_data(200).expect("prune data");
    assert_eq!(report.expired_snapshots, 1);
    assert!(report.deleted_rows > 0);

    let connection = storage.connect().expect("post-prune connection");
    for table in [
        "chronicle",
        "index_entries",
        "kg_entities",
        "kg_relations",
        "workflows",
        "wiki_pages",
        "salient_flags",
    ] {
        assert_eq!(count(&connection, table), 0, "{table} must be invalidated");
    }
    for table in [
        "snapshots",
        "activity_events",
        "working_memory",
        "recording_events",
        "inline_rewrite_uses",
        "chat_turns",
        "inline_outputs",
        "time_ledger",
        "style_notes",
    ] {
        assert_eq!(
            count(&connection, table),
            1,
            "{table} keeps its current row"
        );
    }
    assert_eq!(
        connection
            .query_row("SELECT snapshot_id FROM snapshots", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("remaining snapshot"),
        "current"
    );
    assert_eq!(
        connection
            .query_row("SELECT nudge_id FROM nudges", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("remaining reminder"),
        "reminder"
    );
    assert_eq!(count(&connection, "proactive_rules"), 1);
    assert_eq!(count(&connection, "time_rules"), 1);
    assert_eq!(
        connection
            .query_row("SELECT source FROM time_rules", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("manual rule"),
        "user"
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("schema version"),
        SCHEMA_VERSION
    );
    drop(connection);

    let hits = storage
        .search_snapshots("fresh retained", 10)
        .expect("search retained capture");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].snapshot_id, "current");
    assert!(storage
        .search_snapshots("expired private", 10)
        .expect("search expired capture")
        .is_empty());

    assert_eq!(
        storage
            .prune_expired_data(200)
            .expect("idempotent prune")
            .deleted_rows,
        0
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn expiring_chat_invalidates_memory_even_without_expired_snapshots() {
    let (storage, directory) = fixture();
    let connection = storage.connect().expect("connection");
    connection
        .execute_batch(
            "INSERT INTO chat_turns (role, content, created_at)
             VALUES ('user', 'old private conversation', 100);
             INSERT INTO chronicle
                 (chronicle_id, level, period_key, summary_text, generated_at)
             VALUES ('summary', 'day', 'fixture', 'derived conversation', 300);
             INSERT INTO index_entries
                 (topic, entities, snapshot_ids, created_at, last_updated_at)
             VALUES ('derived topic', '[]', '[]', 300, 300);
             INSERT INTO wiki_pages
                 (slug, page_type, title, first_seen, last_seen, updated_at)
             VALUES ('derived', 'topic', 'Derived Page', 300, 300, 300);
             INSERT INTO time_rules (project, source, created_at)
             VALUES ('Generated', 'model', 300);",
        )
        .expect("seed chat-derived memory");
    drop(connection);

    let report = storage.prune_expired_data(200).expect("prune data");
    assert_eq!(report.expired_snapshots, 0);
    assert_eq!(report.expired_source_rows, 1);
    let connection = storage.connect().expect("post-prune connection");
    for table in [
        "chat_turns",
        "chronicle",
        "index_entries",
        "wiki_pages",
        "time_rules",
    ] {
        assert_eq!(count(&connection, table), 0, "{table} must be cleared");
    }
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn reminder_storage_rejects_non_local_timezone() {
    use woof_storage::{ProactiveRule, StorageError};

    let (storage, directory) = fixture();
    let result = storage.save_proactive_rule(ProactiveRule {
        rule_id: None,
        label: "Fixture".to_string(),
        prompt: "Fixture".to_string(),
        schedule_kind: "daily".to_string(),
        days_of_week: String::new(),
        hour: 9,
        minute: 0,
        interval_minutes: 0,
        timezone: "UTC".to_string(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
        last_fired_at: None,
        fire_at: None,
    });
    assert!(matches!(
        result,
        Err(StorageError::UnsupportedReminderTimezone)
    ));
    let _ = fs::remove_dir_all(directory);
}
