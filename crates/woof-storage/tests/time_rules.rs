use std::{fs, path::PathBuf};

use uuid::Uuid;
use woof_storage::{CaptureRecord, Storage, TimeRuleWrite};

fn fixture() -> (Storage, PathBuf) {
    let unique = Uuid::new_v4().simple();
    let directory =
        std::env::temp_dir().join(format!("woof-time-rules-{}-{unique}", std::process::id()));
    fs::create_dir_all(&directory).expect("fixture directory");
    let storage = Storage::open(directory.join("woof.db")).expect("storage");
    (storage, directory)
}

fn capture(snapshot_id: &str, domain: &str, at: i64) -> CaptureRecord {
    CaptureRecord {
        snapshot_id: Some(snapshot_id.to_string()),
        content: "Synthetic project planning context".to_string(),
        app: "Safari".to_string(),
        window_title: "Project planning".to_string(),
        url: Some(format!("https://{domain}/planning")),
        domain: Some(domain.to_string()),
        captured_at: at,
        last_seen_at: at + 120,
        duration_s: 120.0,
        focused_name: None,
        focused_role: None,
        focused_path: None,
    }
}

#[test]
fn domain_rules_classify_the_root_and_real_subdomains_consistently() {
    let (storage, directory) = fixture();
    let base = 1_800_000_000;
    storage
        .record_capture(&capture("subdomain", "docs.example.test", base), 40)
        .expect("capture");
    storage
        .save_time_rule(
            None,
            &TimeRuleWrite {
                project: "Atlas".to_string(),
                app: None,
                domain: Some("example.test".to_string()),
                title_contains: None,
                source: "suggested".to_string(),
                created_at: base,
            },
        )
        .expect("save rule");

    assert!(storage
        .unmatched_time_segments(base - 1, base + 300, 10)
        .expect("unmatched segments")
        .is_empty());
    let report = storage
        .time_report(base - 86_400, base + 86_400)
        .expect("time report");
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].project, "Atlas");
    assert_eq!(report[0].seconds, 120.0);

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn domain_rule_metacharacters_are_matched_literally() {
    let (storage, directory) = fixture();
    let base = 1_800_000_000;
    storage
        .record_capture(&capture("unrelated", "unrelated.test", base), 40)
        .expect("capture");
    storage
        .save_time_rule(
            None,
            &TimeRuleWrite {
                project: "Wildcard".to_string(),
                app: None,
                domain: Some("%".to_string()),
                title_contains: None,
                source: "suggested".to_string(),
                created_at: base,
            },
        )
        .expect("save rule");

    let unmatched = storage
        .unmatched_time_segments(base - 1, base + 300, 10)
        .expect("unmatched segments");
    assert_eq!(unmatched.len(), 1);
    let report = storage
        .time_report(base - 86_400, base + 86_400)
        .expect("time report");
    assert_eq!(report.len(), 1);
    assert_eq!(report[0].project, "Unclassified");

    fs::remove_dir_all(directory).unwrap();
}
