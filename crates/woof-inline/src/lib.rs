//! Native inline-assistance primitives.
//!
//! Text, clipboard payloads, and replacement content deliberately use
//! redacted `Debug` implementations so diagnostics cannot expose user data.

mod clipboard;
mod error;
mod model;
mod modifier;
mod session;
mod utf16;

#[cfg(target_os = "macos")]
mod macos;

pub use clipboard::{
    with_temporary_text, Clipboard, ClipboardItem, ClipboardRepresentation, ClipboardRevision,
    ClipboardSnapshot,
};
pub use error::{InlineError, PreviewWriteError};
#[cfg(target_os = "macos")]
pub use macos::{
    input_monitoring_trusted, record_modifier_key, record_shortcut_chord, request_input_monitoring,
    MacOsClipboard, MacOsFocusedTarget, MacOsInputInjector, ModifierMonitor, ModifierMonitorHandle,
    RecordedShortcutChord,
};
pub use model::{
    DeliveryFocus, DeliveryMethod, FallbackTarget, FocusedElementMetadata, InlineRead, Rect,
    ReplacementAttempt, TextScope, Utf16Range, WakeHint,
};
pub use modifier::{
    ModifierConfig, ModifierEvent, ModifierInput, ModifierKey, ModifierStateMachine,
};
pub use session::{FocusedTextTarget, InlineSession, InputInjector};
pub use utf16::{replace_utf16_range, slice_utf16_range, utf16_chunks};
