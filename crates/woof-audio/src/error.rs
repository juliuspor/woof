use thiserror::Error;
use woof_llm::RealtimeError;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("microphone capture is only available on macOS")]
    UnsupportedPlatform,
    #[error("microphone permission is denied")]
    PermissionDenied,
    #[error("microphone permission is restricted")]
    PermissionRestricted,
    #[error("could not query microphone permission")]
    PermissionQuery,
    #[error("could not request microphone permission")]
    PermissionRequest,
    #[error("microphone permission request timed out")]
    PermissionRequestTimeout,
    #[error("no microphone input device is available")]
    DeviceUnavailable,
    #[error("could not configure microphone input")]
    StreamConfiguration,
    #[error("microphone input startup timed out")]
    StreamStartupTimeout,
    #[error("microphone input failed")]
    Stream,
    #[error("microphone input exceeded its bounded buffer")]
    BufferOverflow,
    #[error("transcription session ended before accepting audio")]
    SessionClosed,
    #[error("transcription was cancelled")]
    Cancelled,
    #[error("OpenAI Realtime transcription failed")]
    Realtime(#[from] RealtimeError),
}
