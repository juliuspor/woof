//! Privacy-first Accessibility capture primitives.
//!
//! This crate deliberately keeps capture policy, coalescing, redaction, and
//! retry behavior independent from macOS APIs so they can be regression-tested
//! without Accessibility permission.

mod backoff;
mod engine;
mod model;
mod policy;
mod redact;

#[cfg(target_os = "macos")]
pub mod macos;

pub use backoff::ExponentialBackoff;
pub use engine::{
    CaptureController, CapturePipeline, PipelineConfig, PipelineOutcome, SkipReason,
    SnapshotCandidate,
};
pub use model::{
    capture_after_preflight, AccessibilityNode, AccessibilityProvider, CaptureError,
    CaptureMetadata, ForegroundCapture, RawCapture,
};
pub use policy::{BlacklistKind, BlacklistRule, CapturePolicy};
pub use redact::{
    RedactionKind, RedactionReport, RedactionRestoreError, Redactor, RestorableRedaction,
};

/// The bundle identifier excluded unconditionally to prevent woof from
/// recursively capturing its own UI.
pub const WOOF_BUNDLE_ID: &str = "com.julius.woof";
