use std::{fs, path::PathBuf};

use uuid::Uuid;
use woof_storage::{
    CaptureRecord, ChronicleWrite, HourMemoryWrite, ProactiveRule, SalientFlagWrite, Storage,
    WikiPageWrite, SCHEMA_VERSION,
};

fn fixture() -> (Storage, PathBuf) {
    let unique = Uuid::new_v4().simple();
    let directory = std::env::temp_dir().join(format!(
        "woof-derived-writes-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).expect("fixture directory");
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    (storage, directory)
}

fn capture(snapshot_id: &str, at: i64, duration_s: f64) -> CaptureRecord {
    CaptureRecord {
        snapshot_id: Some(snapshot_id.to_string()),
        content: format!("Synthetic planning context {snapshot_id}"),
        app: "TextEdit".to_string(),
        window_title: "Atlas planning".to_string(),
        url: Some("https://example.test/atlas".to_string()),
        domain: Some("example.test".to_string()),
        captured_at: at,
        last_seen_at: at + duration_s as i64,
        duration_s,
        focused_name: Some("Editor".to_string()),
        focused_role: Some("AXTextArea".to_string()),
        focused_path: Some("[\"Window\",\"Editor\"]".to_string()),
    }
}

#[test]
fn dismissed_workflows_stay_hidden_after_reload() {
    let (storage, directory) = fixture();
    let base = 1_800_000_000;
    for (index, offset) in [0, 3_600, 7_200].into_iter().enumerate() {
        storage
            .record_capture(
                &capture(&format!("dismiss-{index}"), base + offset, 120.0),
                40,
            )
            .expect("capture recurring work");
    }

    let workflow_id = storage
        .work_pattern_status(20)
        .expect("work patterns")
        .recent[0]
        .workflow_id
        .clone()
        .expect("workflow ID");
    assert!(storage
        .set_workflow_status(&workflow_id, "dismissed", base + 8_000)
        .expect("dismiss workflow"));

    drop(storage);
    let after_restart = Storage::open(directory.join("woof.db")).expect("reload storage");
    let patterns = after_restart
        .work_pattern_status(20)
        .expect("work patterns");
    assert_eq!(
        patterns.total, 1,
        "dismissal remains durable for deduplication"
    );
    assert_eq!(patterns.by_status.get("dismissed"), Some(&1));
    assert!(patterns.recent.is_empty(), "dismissed cards stay hidden");
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn capture_and_memory_commits_generate_all_derived_production_rows() {
    let (storage, directory) = fixture();
    let base = 1_800_000_000;

    storage
        .record_capture(&capture("capture-a", base, 120.0), 40)
        .expect("first capture");
    storage
        .record_capture(&capture("capture-a", base, 180.0), 40)
        .expect("coalesced capture update");
    storage
        .record_capture(&capture("capture-b", base + 3_600, 120.0), 40)
        .expect("second capture");
    storage
        .record_capture(&capture("capture-c", base + 7_200, 120.0), 40)
        .expect("third capture");

    let connection = storage.connect().expect("connection");
    let ledger_seconds: f64 = connection
        .query_row("SELECT sum(seconds) FROM time_ledger", [], |row| row.get(0))
        .expect("time ledger");
    assert_eq!(
        ledger_seconds, 420.0,
        "only cumulative duration deltas count"
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM workflows", [], |row| row
                .get::<_, i64>(0))
            .expect("workflow count"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM nudges WHERE dedupe_key LIKE 'workflow:%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("workflow nudge"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT deep_link FROM nudges WHERE dedupe_key LIKE 'workflow:%'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("workflow deep link"),
        "woof://memory-hub/workflows"
    );
    drop(connection);

    let patterns = storage.work_pattern_status(20).expect("work patterns");
    let workflow = patterns.recent.first().expect("detected workflow");
    assert_eq!(workflow.status, "proposed");
    assert_eq!(workflow.observations.len(), 3);
    assert!(workflow.frequency_label.contains("recurrences"));
    let workflow_id = workflow.workflow_id.as_deref().expect("workflow ID");
    assert!(storage
        .set_workflow_status(workflow_id, "accepted", base + 8_000)
        .expect("accept workflow"));
    assert!(!storage
        .set_workflow_status(workflow_id, "dismissed", base + 8_001)
        .expect("terminal workflow is idempotent"));
    assert_eq!(
        storage.work_pattern_status(20).unwrap().recent[0].status,
        "accepted"
    );

    let memory = HourMemoryWrite {
        chronicle: ChronicleWrite {
            chronicle_id: "hour-1".to_string(),
            level: "hour".to_string(),
            period_key: "2030-01-15T08".to_string(),
            summary_text: "Atlas planning with Ada".to_string(),
            snapshot_ids: "[\"capture-a\",\"capture-b\"]".to_string(),
            child_ids: "[]".to_string(),
            token_count: Some(12),
            generated_at: base + 600,
            model_used: "synthetic-test-model".to_string(),
        },
        wiki_pages: vec![
            WikiPageWrite {
                slug: "atlas".to_string(),
                page_type: "project".to_string(),
                title: "Atlas".to_string(),
                aliases: "[]".to_string(),
                summary: "Synthetic project".to_string(),
                body: "Planning notes".to_string(),
                links: "[\"Ada\"]".to_string(),
                snapshot_ids: "[\"capture-a\"]".to_string(),
                mention_count: 2,
                first_seen: base,
                last_seen: base + 600,
                updated_at: base + 600,
                model_used: "synthetic-test-model".to_string(),
            },
            WikiPageWrite {
                slug: "ada".to_string(),
                page_type: "person".to_string(),
                title: "Ada".to_string(),
                aliases: "[]".to_string(),
                summary: "Synthetic collaborator".to_string(),
                body: "Works on Atlas".to_string(),
                links: "[\"Atlas\"]".to_string(),
                snapshot_ids: "[\"capture-b\"]".to_string(),
                mention_count: 2,
                first_seen: base + 180,
                last_seen: base + 600,
                updated_at: base + 600,
                model_used: "synthetic-test-model".to_string(),
            },
        ],
        flags: vec![SalientFlagWrite {
            kind: "commitment".to_string(),
            text: "Send Ada the Atlas draft".to_string(),
            snapshot_id: Some("capture-a".to_string()),
            period_key: "2030-01-15T08".to_string(),
            created_at: base + 600,
        }],
        time_rules: Vec::new(),
    };
    assert!(storage.commit_hour_memory(&memory).expect("commit memory"));
    assert!(!storage
        .commit_hour_memory(&memory)
        .expect("idempotent memory commit"));

    let connection = storage.connect().expect("connection");
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM index_entries", [], |row| row
                .get::<_, i64>(0))
            .expect("index entries"),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM kg_relations", [], |row| row
                .get::<_, i64>(0))
            .expect("knowledge graph relations"),
        2
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM nudges WHERE dedupe_key LIKE 'salient_flag:%'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("salient nudge"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT deep_link FROM nudges WHERE dedupe_key LIKE 'salient_flag:%'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("follow-up deep link"),
        "woof://memory-hub/followups"
    );
    let followup_id = connection
        .query_row(
            "SELECT flag_id FROM salient_flags WHERE text='Send Ada the Atlas draft'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("follow-up ID");
    drop(connection);

    assert!(storage
        .set_followup_status(followup_id, "resolved", base + 601)
        .expect("resolve follow-up"));
    assert!(!storage
        .set_followup_status(followup_id, "dismissed", base + 602)
        .expect("terminal follow-up is idempotent"));
    assert!(storage.followups(Some("open"), 20).unwrap().is_empty());
    assert_eq!(storage.followups(Some("resolved"), 20).unwrap().len(), 1);
    let connection = storage.connect().expect("connection");
    assert_eq!(
        connection
            .query_row(
                "SELECT status FROM nudges WHERE dedupe_key = ?1",
                [format!("salient_flag:{followup_id}")],
                |row| row.get::<_, String>(0),
            )
            .expect("resolved follow-up nudge"),
        "dismissed"
    );
    drop(connection);

    let rule = storage
        .save_proactive_rule(ProactiveRule {
            rule_id: Some("one-shot".to_string()),
            label: "Review Atlas".to_string(),
            prompt: "Open the Atlas follow-up".to_string(),
            schedule_kind: "once".to_string(),
            days_of_week: String::new(),
            hour: 9,
            minute: 0,
            interval_minutes: 0,
            timezone: "local".to_string(),
            enabled: true,
            created_at: base,
            updated_at: base,
            last_fired_at: None,
            fire_at: Some(base + 700),
        })
        .expect("save rule");
    assert_eq!(rule.rule_id.as_deref(), Some("one-shot"));
    assert_eq!(
        storage
            .materialize_due_rule_nudges(base + 701, 20)
            .expect("materialize rule"),
        1
    );
    assert_eq!(
        storage
            .materialize_due_rule_nudges(base + 701, 20)
            .expect("deduplicated materialization"),
        0
    );
    assert!(storage
        .delete_proactive_rule("one-shot")
        .expect("delete scheduled rule"));
    let connection = storage.connect().expect("connection");
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM nudges WHERE kind='scheduled_rule'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("cancelled scheduled nudges"),
        0
    );
    drop(connection);
    let rule = storage
        .save_proactive_rule(ProactiveRule {
            rule_id: Some("one-shot".to_string()),
            label: "Review Atlas".to_string(),
            prompt: "Open the Atlas follow-up".to_string(),
            schedule_kind: "once".to_string(),
            days_of_week: String::new(),
            hour: 9,
            minute: 0,
            interval_minutes: 0,
            timezone: "local".to_string(),
            enabled: true,
            created_at: base,
            updated_at: base,
            last_fired_at: None,
            fire_at: Some(base + 700),
        })
        .expect("restore rule");
    assert_eq!(rule.rule_id.as_deref(), Some("one-shot"));
    assert_eq!(
        storage
            .materialize_due_rule_nudges(base + 701, 20)
            .expect("rematerialize rule"),
        1
    );
    assert!(
        storage
            .promote_due_nudges(base + 701)
            .expect("promote nudges")
            >= 1
    );

    assert_eq!(
        storage
            .record_inline_use("TextEdit", "example.test", base + 702)
            .expect("record inline interaction"),
        1
    );

    let connection = storage.connect().expect("connection");
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM nudges WHERE kind='scheduled_rule' AND status='ready'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("scheduled nudge"),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT last_fired_at FROM proactive_rules WHERE rule_id='one-shot'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("last fired"),
        base + 700
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM recording_events", [], |row| row
                .get::<_, i64>(0))
            .expect("recording event"),
        1
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .expect("schema version"),
        SCHEMA_VERSION
    );
    drop(connection);
    assert!(storage
        .delete_proactive_rule("one-shot")
        .expect("delete rule with ready nudge"));
    assert_eq!(
        storage
            .connect()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM nudges WHERE kind='scheduled_rule'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("ready scheduled nudge cancelled"),
        0
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn a_single_long_capture_is_not_a_recurring_workflow() {
    let (storage, directory) = fixture();
    storage
        .record_capture(&capture("one-long-session", 1_800_000_000, 3_600.0), 40)
        .expect("long capture");
    assert_eq!(storage.work_pattern_status(20).unwrap().total, 0);
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn due_rules_are_not_starved_by_earlier_rules_that_are_not_due() {
    let (storage, directory) = fixture();
    let now = 1_800_000_000;
    for (index, fire_at) in [(0, now + 3_600), (1, now + 7_200), (2, now - 1)] {
        storage
            .save_proactive_rule(ProactiveRule {
                rule_id: Some(format!("rule-{index}")),
                label: format!("Rule {index}"),
                prompt: format!("Review item {index}"),
                schedule_kind: "once".to_string(),
                days_of_week: String::new(),
                hour: 0,
                minute: 0,
                interval_minutes: 0,
                timezone: "local".to_string(),
                enabled: true,
                created_at: now + index,
                updated_at: now + index,
                last_fired_at: None,
                fire_at: Some(fire_at),
            })
            .expect("save rule");
    }

    assert_eq!(
        storage
            .materialize_due_rule_nudges(now, 1)
            .expect("materialize one due rule"),
        1
    );
    let connection = storage.connect().expect("connection");
    let (body, deep_link): (String, String) = connection
        .query_row(
            "SELECT body, deep_link FROM nudges WHERE dedupe_key = 'scheduled_rule:rule-2:1799999999'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("due nudge");
    assert_eq!(body, "Review item 2");
    let parsed = url::Url::parse(&deep_link).expect("chat deep link");
    assert_eq!(parsed.scheme(), "woof");
    assert_eq!(parsed.host_str(), Some("chat"));
    assert_eq!(
        parsed.query_pairs().collect::<Vec<_>>(),
        vec![("prompt".into(), "Review item 2".into())]
    );
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn simultaneous_nudges_remain_retrievable_until_seen_or_distinctly_dismissed() {
    let (storage, directory) = fixture();
    let now = 1_800_000_000;
    let delivered_ids = [
        "0194f3cb-16d8-7f10-a922-4379a7c54d31",
        "0194f3cb-16d8-7f10-a922-4379a7c54d32",
    ];
    let failed_id = "0194f3cb-16d8-7f10-a922-4379a7c54d33";
    let connection = storage.connect().expect("connection");
    for (index, nudge_id) in delivered_ids.iter().chain([&failed_id]).enumerate() {
        connection
            .execute(
                "INSERT INTO nudges
                 (nudge_id, kind, scheduled_for, title, body, deep_link, status, created_at)
                 VALUES (?1, 'scheduled_rule', ?2, ?3, ?4, ?5, 'pending', ?2)",
                rusqlite::params![
                    nudge_id,
                    now,
                    format!("Reminder {index}"),
                    format!("Private prompt {index}"),
                    format!("woof://chat?prompt=Private%20prompt%20{index}"),
                ],
            )
            .expect("seed nudge");
    }
    drop(connection);

    let ready = storage
        .ready_nudges(now, 50)
        .expect("ready simultaneous nudges");
    assert_eq!(ready.len(), 3);
    assert!(ready.iter().all(|nudge| nudge.sent_at.is_none()));
    for nudge_id in delivered_ids {
        assert!(storage
            .mark_nudge_delivered(nudge_id, now + 1)
            .expect("mark successful delivery"));
    }

    drop(storage);
    let reopened = Storage::open(directory.join("woof.db")).expect("reopen storage");
    let after_restart = reopened.ready_nudges(now + 2, 50).expect("reload nudges");
    assert_eq!(after_restart.len(), 3, "delivered rows survive a restart");
    assert!(after_restart.iter().any(|nudge| {
        nudge.nudge_id.as_deref() == Some(delivered_ids[0]) && nudge.sent_at == Some(now + 1)
    }));
    assert_eq!(
        reopened.ready_nudges(now + 2, 1).unwrap()[0]
            .nudge_id
            .as_deref(),
        Some(failed_id),
        "an undelivered row is retained and prioritized for retry"
    );

    assert!(reopened
        .mark_nudge_seen(delivered_ids[0], now + 3)
        .expect("mark opened nudge seen"));
    assert!(reopened
        .dismiss_nudge(delivered_ids[1], now + 4)
        .expect("dismiss second nudge"));
    let connection = reopened.connect().expect("connection");
    let states = delivered_ids
        .iter()
        .map(|nudge_id| {
            connection
                .query_row(
                    "SELECT status FROM nudges WHERE nudge_id = ?1",
                    [nudge_id],
                    |row| row.get::<_, String>(0),
                )
                .expect("nudge state")
        })
        .collect::<Vec<_>>();
    assert_eq!(states, ["seen", "dismissed"]);
    assert!(reopened.ready_nudge(failed_id).unwrap().is_some());
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn proactive_rule_count_is_bounded() {
    let (storage, directory) = fixture();
    storage
        .connect()
        .expect("connection")
        .execute_batch(
            "WITH RECURSIVE sequence(value) AS (
                 SELECT 1 UNION ALL SELECT value + 1 FROM sequence WHERE value < 500
             )
             INSERT INTO proactive_rules
                 (rule_id, label, prompt, schedule_kind, timezone, enabled, created_at, updated_at)
             SELECT 'seed-' || value, 'Seed', 'Seed', 'daily', 'local', 0, value, value
             FROM sequence;",
        )
        .expect("seed bounded rules");
    let result = storage.save_proactive_rule(ProactiveRule {
        rule_id: Some("one-too-many".to_string()),
        label: "One too many".to_string(),
        prompt: "This rule must be rejected".to_string(),
        schedule_kind: "daily".to_string(),
        days_of_week: String::new(),
        hour: 9,
        minute: 0,
        interval_minutes: 0,
        timezone: "local".to_string(),
        enabled: true,
        created_at: 1_800_000_000,
        updated_at: 1_800_000_000,
        last_fired_at: None,
        fire_at: None,
    });
    assert!(result.is_err());
    let _ = fs::remove_dir_all(directory);
}
