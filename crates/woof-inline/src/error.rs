use thiserror::Error;

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum InlineError {
    #[error("inline assistance is only available on macOS")]
    UnsupportedPlatform,
    #[error("Accessibility permission has not been granted")]
    PermissionDenied,
    #[error("secure keyboard input is active")]
    SecureInput,
    #[error("the focused element contains protected content")]
    ProtectedContent,
    #[error("there is no focused editable element")]
    NoFocusedElement,
    #[error("the focused element was released")]
    Released,
    #[error("the focused element does not expose readable text")]
    TextUnavailable,
    #[error("the focused inline target changed before delivery")]
    TargetFocusChanged,
    #[error("the inline target content or selection changed before delivery")]
    TargetContentChanged,
    #[error("the focused element is not writable through Accessibility")]
    NotWritable,
    #[error("the Accessibility operation failed")]
    Accessibility,
    #[error("the UTF-16 selection range is invalid")]
    InvalidRange,
    #[error("the clipboard could not be snapshotted exactly")]
    ClipboardSnapshot,
    #[error("the clipboard contents exceed the safe inline-delivery limit")]
    ClipboardLimit,
    #[error("the clipboard changed during inline delivery")]
    ClipboardChanged,
    #[error("temporary clipboard text could not be written")]
    ClipboardWrite,
    #[error("the original clipboard could not be restored")]
    ClipboardRestore,
    #[error("synthetic keyboard input failed")]
    InputInjection,
    #[error("the modifier event monitor could not start")]
    EventMonitor,
    #[error("shortcut recording timed out")]
    RecordingTimeout,
    #[error("unsupported shortcut recording key")]
    UnsupportedRecordingKey,
}
