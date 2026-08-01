//! OpenAI-only networking for woof.
//!
//! Production constructors do not accept a custom base URL. Tests inject an
//! in-memory transport, making accidental live requests impossible.

mod cancel;
mod chat;
mod endpoint;
mod error;
mod keychain;
mod realtime;

pub use cancel::CancellationToken;
pub use chat::{
    AssistantFunctionCall, AssistantToolCall, ByteStream, ChatClient, ChatCompletion, ChatMessage,
    ChatRequest, ChatRole, ChatStreamEvent, ChatTool, ChatTransport, FunctionDefinition,
    FunctionToolCall, HttpsChatTransport, ReasoningEffort, TokenUsage,
};
pub use endpoint::{
    validate_openai_url, CHAT_COMPLETIONS_URL, OPENAI_HOST, REALTIME_TRANSCRIPTION_URL,
};
pub use error::{ChatError, KeyStoreError, RealtimeError, TransportError};
pub use keychain::{
    ApiKey, MacOsKeychain, OpenAiKeyStore, OPENAI_KEYCHAIN_ACCOUNT, OPENAI_KEYCHAIN_SERVICE,
};
pub use realtime::{
    AudioCommand, RealtimeSessionConfig, RealtimeTranscriptionClient, TranscriptReconciler,
    TranscriptionEvent, TRANSCRIPTION_MODEL,
};

pub const CHAT_MODEL: &str = "gpt-5.6-terra";
