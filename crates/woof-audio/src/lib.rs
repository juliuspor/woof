//! Microphone capture and OpenAI Realtime transcription orchestration.
//!
//! The crate has no credential store. A caller lends an [`ApiKey`] to a
//! session, and the session neither clones nor persists it.

mod error;
mod permission;
mod session;
mod source;

#[cfg(target_os = "macos")]
mod macos;

pub use error::AudioError;
#[cfg(target_os = "macos")]
pub use macos::{MacOsMicrophone, MicrophoneStopHandle};
pub use permission::{
    microphone_authorization, request_microphone_authorization,
    request_microphone_authorization_with_cancellation, MicrophoneAuthorization,
};
pub use session::{
    AudioEvent, BackendEventDisposition, BackendEventSender, OpenAiRealtimeBackend,
    RealtimeBackend, TranscriptionOutcome, TranscriptionSession,
};
pub use source::{
    AudioFrame, AudioSource, Pcm16Resampler, TRANSCRIPTION_CHANNELS, TRANSCRIPTION_SAMPLE_RATE,
};
pub use woof_llm::{ApiKey, CancellationToken, RealtimeSessionConfig, TRANSCRIPTION_MODEL};
