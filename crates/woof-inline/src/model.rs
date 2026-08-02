use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Utf16Range {
    pub location: usize,
    pub length: usize,
}

impl Utf16Range {
    pub fn end(self) -> Option<usize> {
        self.location.checked_add(self.length)
    }

    pub fn is_empty(self) -> bool {
        self.length == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextScope {
    Selection,
    WholeDraft,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WakeHint {
    #[default]
    Standard,
    GmailContentEditable,
}

#[derive(Clone, Default, PartialEq)]
pub struct FocusedElementMetadata {
    pub pid: i32,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub window_id: Option<i64>,
    pub role: String,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub identifier: Option<String>,
    pub frame: Option<Rect>,
    pub selection: Option<Utf16Range>,
    pub selected_text_writable: bool,
    pub value_writable: bool,
    pub contenteditable: bool,
}

impl fmt::Debug for FocusedElementMetadata {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FocusedElementMetadata")
            .field("pid", &self.pid)
            .field("bundle_id", &self.bundle_id)
            .field(
                "window_title",
                &self.window_title.as_ref().map(|_| "[REDACTED]"),
            )
            .field("window_id", &self.window_id)
            .field("role", &self.role)
            .field("subrole", &self.subrole)
            .field("title", &self.title.as_ref().map(|_| "[REDACTED]"))
            .field(
                "description",
                &self.description.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "identifier",
                &self.identifier.as_ref().map(|_| "[REDACTED]"),
            )
            .field("frame", &self.frame)
            .field("selection", &self.selection)
            .field("selected_text_writable", &self.selected_text_writable)
            .field("value_writable", &self.value_writable)
            .field("contenteditable", &self.contenteditable)
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub struct InlineRead {
    pub scope: TextScope,
    pub text: String,
    pub selection: Option<Utf16Range>,
    pub metadata: FocusedElementMetadata,
}

/// Which foreground context may authorize an inline delivery.
///
/// Dictation always requires the retained target itself. Rewrite delivery may
/// additionally accept the already-verified woof controller process long
/// enough to hide its edit overlay and restore focus to the retained target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryFocus {
    Target,
    ControllerOrTarget { controller_pid: i32 },
}

/// Exact native destination prepared for a keyboard fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FallbackTarget {
    pub pid: i32,
    pub selection: Utf16Range,
}

impl fmt::Debug for InlineRead {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InlineRead")
            .field("scope", &self.scope)
            .field("text", &"[REDACTED]")
            .field("utf8_bytes", &self.text.len())
            .field("selection", &self.selection)
            .field("metadata", &self.metadata)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplacementAttempt {
    SelectedText,
    Value,
    ValueRange,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryMethod {
    AccessibilitySelectedText,
    AccessibilityValue,
    AccessibilityValueRange,
    ClipboardPaste,
    UnicodeKeystrokes,
}

impl From<ReplacementAttempt> for Option<DeliveryMethod> {
    fn from(attempt: ReplacementAttempt) -> Self {
        match attempt {
            ReplacementAttempt::SelectedText => Some(DeliveryMethod::AccessibilitySelectedText),
            ReplacementAttempt::Value => Some(DeliveryMethod::AccessibilityValue),
            ReplacementAttempt::ValueRange => Some(DeliveryMethod::AccessibilityValueRange),
            ReplacementAttempt::Unavailable => None,
        }
    }
}
