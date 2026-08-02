use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::{CapturePolicy, RawCapture, Redactor};

const HARD_MAXIMUM_TEXT_BYTES: usize = 1024 * 1024;
const MAXIMUM_APP_NAME_BYTES: usize = 512;
const MAXIMUM_BUNDLE_ID_BYTES: usize = 512;
const MAXIMUM_WINDOW_TITLE_BYTES: usize = 4 * 1024;
const MAXIMUM_BROWSER_URL_BYTES: usize = 8 * 1024;
const MAXIMUM_FOCUSED_PATH_BYTES: usize = 4 * 1024;
const MAXIMUM_FOCUSED_PATH_SEGMENT_BYTES: usize = 512;
const MAXIMUM_FOCUSED_ROLE_BYTES: usize = 256;

#[derive(Clone, Debug)]
pub struct PipelineConfig {
    pub coalesce_window_ms: i64,
    pub maximum_text_bytes: usize,
    pub policy: CapturePolicy,
    pub redactor: Redactor,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            coalesce_window_ms: 30_000,
            maximum_text_bytes: 256 * 1024,
            policy: CapturePolicy::default(),
            redactor: Redactor::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCandidate {
    pub started_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub duration_ms: i64,
    pub pid: i32,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub browser_url: Option<String>,
    pub focused_breadcrumbs: Vec<String>,
    pub focused_role: Option<String>,
    pub text: String,
    pub content_hash: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    Paused,
    SecureInput,
    ProtectedContent,
    Blacklisted,
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineOutcome {
    Stored(SnapshotCandidate),
    /// The foreground context was unchanged and only its observed duration grew.
    Deduplicated(SnapshotCandidate),
    /// The foreground context stayed the same but its visible text changed.
    Coalesced(SnapshotCandidate),
    Skipped(SkipReason),
}

#[derive(Clone, Debug)]
pub struct CaptureController {
    paused: Arc<AtomicBool>,
    continuity_epoch: Arc<AtomicU64>,
}

impl Default for CaptureController {
    fn default() -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            continuity_epoch: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl CaptureController {
    pub fn pause(&self) {
        if !self.paused.swap(true, Ordering::SeqCst) {
            self.continuity_epoch.fetch_add(1, Ordering::SeqCst);
        }
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Changes whenever capture transitions into a paused interval, including
    /// pauses that begin and end between two capture attempts.
    pub fn continuity_epoch(&self) -> u64 {
        self.continuity_epoch.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug)]
pub struct CapturePipeline {
    config: PipelineConfig,
    controller: CaptureController,
    observed_continuity_epoch: u64,
    pending: Option<SnapshotCandidate>,
}

impl CapturePipeline {
    pub fn new(config: PipelineConfig, controller: CaptureController) -> Self {
        let observed_continuity_epoch = controller.continuity_epoch();
        Self {
            config,
            controller,
            observed_continuity_epoch,
            pending: None,
        }
    }

    pub fn process(&mut self, mut capture: RawCapture) -> PipelineOutcome {
        self.clear_for_controller_discontinuity();
        if self.controller.is_paused() {
            capture.zeroize_sensitive();
            return self.skip(SkipReason::Paused);
        }
        if capture.secure_input {
            capture.zeroize_sensitive();
            return self.skip(SkipReason::SecureInput);
        }
        if capture.root.has_protected_content() {
            capture.zeroize_sensitive();
            return self.skip(SkipReason::ProtectedContent);
        }
        if self.config.policy.is_blacklisted(&capture) {
            capture.zeroize_sensitive();
            return self.skip(SkipReason::Blacklisted);
        }

        let maximum_text_bytes = self.config.maximum_text_bytes.min(HARD_MAXIMUM_TEXT_BYTES);
        let mut unredacted = capture.root.visible_text_bounded(maximum_text_bytes);
        let redacted = self.config.redactor.redact(&unredacted).text;
        unredacted.zeroize();
        let text = truncate_utf8(&redacted, maximum_text_bytes);
        if text.trim().is_empty() {
            return self.skip(SkipReason::Empty);
        }
        let content_hash: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        let focused_breadcrumbs =
            bounded_redacted_breadcrumbs(&self.config.redactor, capture.root.focused_breadcrumbs());
        let focused_role = capture
            .root
            .focused_role()
            .map(|value| truncate_utf8(value, MAXIMUM_FOCUSED_ROLE_BYTES));
        let app_name = truncate_owned_utf8(
            std::mem::take(&mut capture.app_name),
            MAXIMUM_APP_NAME_BYTES,
        );
        let bundle_id = capture
            .bundle_id
            .take()
            .map(|value| truncate_owned_utf8(value, MAXIMUM_BUNDLE_ID_BYTES));
        let window_title = capture.window_title.take().map(|value| {
            redact_owned_bounded(&self.config.redactor, value, MAXIMUM_WINDOW_TITLE_BYTES)
        });
        let browser_url = capture.browser_url.take().map(|value| {
            redact_owned_bounded(&self.config.redactor, value, MAXIMUM_BROWSER_URL_BYTES)
        });
        capture.zeroize_sensitive();
        let mut next = SnapshotCandidate {
            started_at_ms: capture.captured_at_ms,
            last_seen_at_ms: capture.captured_at_ms,
            duration_ms: 0,
            pid: capture.pid,
            app_name,
            bundle_id,
            window_title,
            browser_url,
            focused_breadcrumbs,
            focused_role,
            text,
            content_hash,
        };

        let Some(previous) = self.pending.as_ref() else {
            self.pending = Some(next.clone());
            return PipelineOutcome::Stored(next);
        };

        let elapsed = next
            .last_seen_at_ms
            .saturating_sub(previous.last_seen_at_ms)
            .max(0);
        let same_context = previous.pid == next.pid
            && previous.bundle_id == next.bundle_id
            && previous.window_title == next.window_title
            && previous.browser_url == next.browser_url;
        let inside_window = elapsed <= self.config.coalesce_window_ms;

        if same_context && inside_window {
            next.started_at_ms = previous.started_at_ms;
            next.duration_ms = next
                .last_seen_at_ms
                .saturating_sub(previous.started_at_ms)
                .max(previous.duration_ms);
            let duplicate = previous.content_hash == next.content_hash;
            self.pending = Some(next.clone());
            if duplicate {
                PipelineOutcome::Deduplicated(next)
            } else {
                PipelineOutcome::Coalesced(next)
            }
        } else {
            self.pending = Some(next.clone());
            PipelineOutcome::Stored(next)
        }
    }

    pub fn pending(&self) -> Option<&SnapshotCandidate> {
        self.pending.as_ref()
    }

    /// Ends the current foreground sequence so the next accepted capture is a
    /// fresh snapshot with duration accounting starting at that observation.
    pub fn clear_pending(&mut self) {
        self.pending = None;
    }

    /// Replaces foreground filtering rules without discarding the pending
    /// snapshot used for duration accounting and coalescing.
    pub fn set_policy(&mut self, policy: CapturePolicy) {
        self.config.policy = policy;
    }

    fn clear_for_controller_discontinuity(&mut self) {
        let epoch = self.controller.continuity_epoch();
        if epoch != self.observed_continuity_epoch {
            self.clear_pending();
            self.observed_continuity_epoch = epoch;
        }
    }

    fn skip(&mut self, reason: SkipReason) -> PipelineOutcome {
        self.clear_pending();
        PipelineOutcome::Skipped(reason)
    }
}

fn truncate_utf8(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value[..boundary].to_owned()
}

fn truncate_owned_utf8(mut value: String, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value.truncate(boundary);
    value
}

fn redact_owned_bounded(redactor: &Redactor, mut value: String, maximum_bytes: usize) -> String {
    let redacted = redactor.redact(&value).text;
    value.zeroize();
    truncate_owned_utf8(redacted, maximum_bytes)
}

fn bounded_redacted_breadcrumbs(redactor: &Redactor, values: Vec<String>) -> Vec<String> {
    let mut output = Vec::new();
    let mut used_bytes = 0_usize;
    for value in values {
        let value = redact_owned_bounded(redactor, value, MAXIMUM_FOCUSED_PATH_SEGMENT_BYTES);
        let separator_bytes = usize::from(!output.is_empty());
        let next_bytes = used_bytes
            .saturating_add(separator_bytes)
            .saturating_add(value.len());
        if next_bytes > MAXIMUM_FOCUSED_PATH_BYTES {
            break;
        }
        used_bytes = next_bytes;
        output.push(value);
    }
    output
}

#[cfg(test)]
mod tests {
    use crate::{AccessibilityNode, BlacklistKind, BlacklistRule};

    use super::*;

    fn raw(at: i64, value: &str) -> RawCapture {
        RawCapture {
            captured_at_ms: at,
            pid: 42,
            app_name: "TextEdit".into(),
            bundle_id: Some("com.apple.TextEdit".into()),
            window_title: Some("Synthetic".into()),
            window_id: None,
            browser_url: None,
            secure_input: false,
            root: AccessibilityNode {
                role: "AXWindow".into(),
                children: vec![AccessibilityNode {
                    role: "AXTextArea".into(),
                    value: Some(value.into()),
                    focused: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
        }
    }

    #[test]
    fn coalesces_and_accounts_duration() {
        let controller = CaptureController::default();
        let mut pipeline = CapturePipeline::new(PipelineConfig::default(), controller);
        assert!(matches!(
            pipeline.process(raw(1_000, "one")),
            PipelineOutcome::Stored(_)
        ));
        let dedup = pipeline.process(raw(2_500, "one"));
        assert!(matches!(dedup, PipelineOutcome::Deduplicated(_)));
        let changed = pipeline.process(raw(3_000, "two"));
        let PipelineOutcome::Coalesced(candidate) = changed else {
            panic!("expected coalescing");
        };
        assert_eq!(candidate.started_at_ms, 1_000);
        assert_eq!(candidate.duration_ms, 2_000);
    }

    fn assert_fresh_capture(pipeline: &mut CapturePipeline, at: i64) {
        let PipelineOutcome::Stored(candidate) = pipeline.process(raw(at, "one")) else {
            panic!("expected a fresh stored capture after the discontinuity");
        };
        assert_eq!(candidate.started_at_ms, at);
        assert_eq!(candidate.last_seen_at_ms, at);
        assert_eq!(candidate.duration_ms, 0);
    }

    #[test]
    fn every_skipped_capture_breaks_coalescing_and_duration_accounting() {
        let controller = CaptureController::default();
        let mut paused = CapturePipeline::new(PipelineConfig::default(), controller.clone());
        assert!(matches!(
            paused.process(raw(1_000, "one")),
            PipelineOutcome::Stored(_)
        ));
        controller.pause();
        assert_eq!(
            paused.process(raw(10_000, "one")),
            PipelineOutcome::Skipped(SkipReason::Paused)
        );
        assert!(paused.pending().is_none());
        controller.resume();
        assert_fresh_capture(&mut paused, 20_000);

        let mut secure =
            CapturePipeline::new(PipelineConfig::default(), CaptureController::default());
        assert!(matches!(
            secure.process(raw(1_000, "one")),
            PipelineOutcome::Stored(_)
        ));
        let mut secure_capture = raw(10_000, "secret");
        secure_capture.secure_input = true;
        assert_eq!(
            secure.process(secure_capture),
            PipelineOutcome::Skipped(SkipReason::SecureInput)
        );
        assert!(secure.pending().is_none());
        assert_fresh_capture(&mut secure, 20_000);

        let mut protected =
            CapturePipeline::new(PipelineConfig::default(), CaptureController::default());
        assert!(matches!(
            protected.process(raw(1_000, "one")),
            PipelineOutcome::Stored(_)
        ));
        let mut protected_capture = raw(10_000, "secret");
        protected_capture.root.children[0].protected = true;
        assert_eq!(
            protected.process(protected_capture),
            PipelineOutcome::Skipped(SkipReason::ProtectedContent)
        );
        assert!(protected.pending().is_none());
        assert_fresh_capture(&mut protected, 20_000);

        let mut blacklisted =
            CapturePipeline::new(PipelineConfig::default(), CaptureController::default());
        assert!(matches!(
            blacklisted.process(raw(1_000, "one")),
            PipelineOutcome::Stored(_)
        ));
        blacklisted.set_policy(CapturePolicy::new([BlacklistRule {
            kind: BlacklistKind::AppName,
            pattern: "textedit".into(),
        }]));
        assert_eq!(
            blacklisted.process(raw(10_000, "one")),
            PipelineOutcome::Skipped(SkipReason::Blacklisted)
        );
        assert!(blacklisted.pending().is_none());
        blacklisted.set_policy(CapturePolicy::default());
        assert_fresh_capture(&mut blacklisted, 20_000);

        let mut empty =
            CapturePipeline::new(PipelineConfig::default(), CaptureController::default());
        assert!(matches!(
            empty.process(raw(1_000, "one")),
            PipelineOutcome::Stored(_)
        ));
        assert_eq!(
            empty.process(raw(10_000, " \n\t ")),
            PipelineOutcome::Skipped(SkipReason::Empty)
        );
        assert!(empty.pending().is_none());
        assert_fresh_capture(&mut empty, 20_000);
    }

    #[test]
    fn a_pause_between_attempts_and_an_explicit_clear_both_start_fresh() {
        let controller = CaptureController::default();
        let mut pipeline = CapturePipeline::new(PipelineConfig::default(), controller.clone());
        assert!(matches!(
            pipeline.process(raw(1_000, "one")),
            PipelineOutcome::Stored(_)
        ));

        controller.pause();
        controller.resume();
        assert_fresh_capture(&mut pipeline, 20_000);

        pipeline.clear_pending();
        assert!(pipeline.pending().is_none());
        assert_fresh_capture(&mut pipeline, 25_000);
    }

    #[test]
    fn refuses_secure_and_protected_input() {
        let controller = CaptureController::default();
        let mut pipeline = CapturePipeline::new(PipelineConfig::default(), controller);
        let mut secure = raw(0, "secret");
        secure.secure_input = true;
        assert_eq!(
            pipeline.process(secure),
            PipelineOutcome::Skipped(SkipReason::SecureInput)
        );

        let mut protected = raw(0, "secret");
        protected.root.children[0].protected = true;
        assert_eq!(
            pipeline.process(protected),
            PipelineOutcome::Skipped(SkipReason::ProtectedContent)
        );
    }

    #[test]
    fn supports_pause_and_blacklist() {
        let controller = CaptureController::default();
        controller.pause();
        let mut pipeline = CapturePipeline::new(PipelineConfig::default(), controller.clone());
        assert_eq!(
            pipeline.process(raw(0, "hello")),
            PipelineOutcome::Skipped(SkipReason::Paused)
        );
        controller.resume();

        let config = PipelineConfig {
            policy: CapturePolicy::new([BlacklistRule {
                kind: BlacklistKind::AppName,
                pattern: "textedit".into(),
            }]),
            ..Default::default()
        };
        let mut pipeline = CapturePipeline::new(config, controller);
        assert_eq!(
            pipeline.process(raw(0, "hello")),
            PipelineOutcome::Skipped(SkipReason::Blacklisted)
        );
    }

    #[test]
    fn policy_updates_preserve_pending_coalescing_state() {
        let controller = CaptureController::default();
        let mut pipeline = CapturePipeline::new(PipelineConfig::default(), controller);
        assert!(matches!(
            pipeline.process(raw(1_000, "hello")),
            PipelineOutcome::Stored(_)
        ));
        pipeline.set_policy(CapturePolicy::default());
        let PipelineOutcome::Deduplicated(candidate) = pipeline.process(raw(2_000, "hello")) else {
            panic!("expected the prior snapshot to remain pending");
        };
        assert_eq!(candidate.started_at_ms, 1_000);
    }

    #[test]
    fn redacts_before_candidate_leaves_pipeline() {
        let controller = CaptureController::default();
        let mut pipeline = CapturePipeline::new(PipelineConfig::default(), controller);
        let PipelineOutcome::Stored(candidate) =
            pipeline.process(raw(0, "email me at private@example.com"))
        else {
            panic!("expected stored candidate");
        };
        assert_eq!(candidate.text, "email me at [REDACTED_EMAIL]");
    }

    #[test]
    fn bounds_accessibility_text_before_assembly() {
        let mut capture = raw(0, "discarded");
        capture.root.value = Some("x".repeat(4_096));
        capture.root.children[0].value = Some("kept".into());
        let config = PipelineConfig {
            maximum_text_bytes: 32,
            ..Default::default()
        };
        let mut pipeline = CapturePipeline::new(config, CaptureController::default());
        let PipelineOutcome::Stored(candidate) = pipeline.process(capture) else {
            panic!("expected bounded child content");
        };
        assert_eq!(candidate.text, "kept");
        assert!(candidate.text.len() <= 32);
    }

    #[test]
    fn redacts_and_bounds_sensitive_metadata() {
        let mut capture = raw(0, "draft");
        capture.app_name = "a".repeat(MAXIMUM_APP_NAME_BYTES + 10);
        capture.window_title = Some("Message to private@example.com".into());
        capture.browser_url = Some("https://example.com/?owner=private@example.com".into());
        capture.root.children[0].title = Some("private@example.com".into());

        let mut pipeline =
            CapturePipeline::new(PipelineConfig::default(), CaptureController::default());
        let PipelineOutcome::Stored(candidate) = pipeline.process(capture) else {
            panic!("expected stored candidate");
        };
        assert_eq!(candidate.app_name.len(), MAXIMUM_APP_NAME_BYTES);
        assert_eq!(
            candidate.window_title.as_deref(),
            Some("Message to [REDACTED_EMAIL]")
        );
        assert_eq!(
            candidate.browser_url.as_deref(),
            Some("https://example.com/?owner=[REDACTED_EMAIL]")
        );
        assert!(candidate
            .focused_breadcrumbs
            .iter()
            .any(|value| value == "[REDACTED_EMAIL]"));
    }

    #[test]
    fn preserves_the_focused_accessibility_role_separately_from_breadcrumbs() {
        let controller = CaptureController::default();
        let mut pipeline = CapturePipeline::new(PipelineConfig::default(), controller);
        let mut capture = raw(0, "draft");
        capture.root.children[0].title = Some("Message body".to_string());

        let PipelineOutcome::Stored(candidate) = pipeline.process(capture) else {
            panic!("expected stored candidate");
        };
        assert_eq!(
            candidate.focused_breadcrumbs,
            vec!["AXWindow", "Message body"]
        );
        assert_eq!(candidate.focused_role.as_deref(), Some("AXTextArea"));
    }
}
