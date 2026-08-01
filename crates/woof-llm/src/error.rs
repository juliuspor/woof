use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum KeyStoreError {
    #[error("no OpenAI API key is stored")]
    NotFound,
    #[error("macOS Keychain is unavailable")]
    Unavailable,
    #[error("could not access macOS Keychain")]
    Access,
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("transient OpenAI network error")]
    TransientNetwork,
    #[error("non-retryable OpenAI network error")]
    PermanentNetwork,
    #[error("OpenAI returned HTTP {status}")]
    Http {
        status: u16,
        retry_after: Option<Duration>,
    },
}

impl TransportError {
    pub fn retryable(&self) -> bool {
        match self {
            Self::TransientNetwork => true,
            Self::PermanentNetwork => false,
            Self::Http { status, .. } => *status == 429 || (500..=599).contains(status),
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Http { retry_after, .. } => *retry_after,
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ChatError {
    #[error("request was cancelled")]
    Cancelled,
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("could not encode the OpenAI request")]
    Encode,
    #[error("Chat Completions function tools require reasoning effort none")]
    UnsupportedToolReasoning,
    #[error("OpenAI returned an invalid stream")]
    InvalidStream,
    #[error("OpenAI stream ended before the completion marker")]
    UnexpectedEnd,
    #[error("OpenAI requested too many consecutive tool rounds")]
    ToolRoundsExceeded,
    #[error("stream callback failed")]
    Callback,
}

#[derive(Debug, Error)]
pub enum RealtimeError {
    #[error("request was cancelled")]
    Cancelled,
    #[error("could not connect to OpenAI Realtime")]
    Connection,
    #[error("OpenAI Realtime authentication failed")]
    Authentication,
    #[error("OpenAI Realtime rate limit was reached")]
    RateLimited,
    #[error("OpenAI Realtime returned HTTP {status}")]
    Server { status: u16 },
    #[error("OpenAI Realtime did not finish transcription in time")]
    Timeout,
    #[error("OpenAI Realtime returned an invalid event")]
    InvalidEvent,
    #[error("OpenAI Realtime rejected the transcription session")]
    Rejected,
    #[error("audio channel closed")]
    AudioClosed,
}

impl RealtimeError {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Connection | Self::RateLimited | Self::Server { status: 500..=599 }
        )
    }
}
