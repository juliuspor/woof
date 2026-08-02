use std::{
    ffi::{c_char, c_void},
    ptr,
    sync::atomic::{compiler_fence, Ordering},
    thread,
    time::Duration,
};

use objc2::{
    msg_send,
    rc::autoreleasepool,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::NSString;

use crate::{
    replace_utf16_range, slice_utf16_range, DeliveryFocus, FallbackTarget, FocusedElementMetadata,
    FocusedTextTarget, InlineError, InlineRead, PreviewWriteError, Rect, ReplacementAttempt,
    TextScope, Utf16Range, WakeHint,
};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFAttributedStringRef = *const c_void;
type CFURLRef = *const c_void;
type CFNumberRef = *const c_void;
type CFTypeId = usize;
type CFIndex = isize;
type AXUIElementRef = *const c_void;
type AXValueRef = *const c_void;
type AXError = i32;

const AX_ERROR_SUCCESS: AXError = 0;
const AX_ERROR_INVALID_UI_ELEMENT: AXError = -25202;
const AX_ERROR_CANNOT_COMPLETE: AXError = -25204;
const AX_ERROR_ATTRIBUTE_UNSUPPORTED: AXError = -25205;
const AX_ERROR_API_DISABLED: AXError = -25211;
const AX_ERROR_NO_VALUE: AXError = -25212;
const UTF8: u32 = 0x0800_0100;
const AX_VALUE_CGPOINT: i32 = 1;
const AX_VALUE_CGSIZE: i32 = 2;
const AX_VALUE_CGRECT: i32 = 3;
const AX_VALUE_CFRANGE: i32 = 4;
const CF_NUMBER_SINT64_TYPE: i32 = 4;
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
const MAX_OPERATION_STRING_ALLOCATION_BYTES: usize = MAX_TEXT_BYTES + 1;
const DELIVERY_CONFIRMATION_ATTEMPTS: usize = 8;
const DELIVERY_CONFIRMATION_DELAY: Duration = Duration::from_millis(15);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CFRange {
    location: CFIndex,
    length: CFIndex,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut bool,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
    fn AXUIElementGetTypeID() -> CFTypeId;
    fn AXValueGetTypeID() -> CFTypeId;
    fn AXValueGetType(value: AXValueRef) -> i32;
    fn AXValueGetValue(value: AXValueRef, value_type: i32, output: *mut c_void) -> bool;
    fn AXValueCreate(value_type: i32, value: *const c_void) -> AXValueRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    fn CFRelease(value: CFTypeRef);
    fn CFGetTypeID(value: CFTypeRef) -> CFTypeId;
    fn CFEqual(left: CFTypeRef, right: CFTypeRef) -> bool;
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        byte_count: CFIndex,
        encoding: u32,
        is_external_representation: bool,
    ) -> CFStringRef;
    fn CFStringGetTypeID() -> CFTypeId;
    fn CFStringGetLength(value: CFStringRef) -> CFIndex;
    fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    fn CFStringGetCString(
        value: CFStringRef,
        buffer: *mut c_char,
        buffer_size: CFIndex,
        encoding: u32,
    ) -> bool;
    fn CFAttributedStringGetTypeID() -> CFTypeId;
    fn CFAttributedStringGetString(value: CFAttributedStringRef) -> CFStringRef;
    fn CFURLGetTypeID() -> CFTypeId;
    fn CFURLGetString(value: CFURLRef) -> CFStringRef;
    fn CFBooleanGetTypeID() -> CFTypeId;
    fn CFBooleanGetValue(value: CFTypeRef) -> bool;
    fn CFNumberGetTypeID() -> CFTypeId;
    fn CFNumberGetValue(number: CFNumberRef, number_type: i32, value: *mut c_void) -> bool;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

pub struct MacOsFocusedTarget {
    refs: Option<TargetRefs>,
}

struct TargetRefs {
    application: OwnedAx,
    window: Option<OwnedAx>,
    element: OwnedAx,
    pid: i32,
}

impl MacOsFocusedTarget {
    pub fn acquire() -> Result<Self, InlineError> {
        let mut string_budget = AxStringBudget::default();
        ensure_accessibility_safe()?;
        let system = OwnedAx::new(unsafe { AXUIElementCreateSystemWide() as CFTypeRef })
            .ok_or(InlineError::NoFocusedElement)?;
        let application =
            copy_element(system.0, "AXFocusedApplication")?.ok_or(InlineError::NoFocusedElement)?;
        let element = copy_element(application.0, "AXFocusedUIElement")?
            .ok_or(InlineError::NoFocusedElement)?;
        let window = copy_element(application.0, "AXFocusedWindow")?;
        let mut pid = 0;
        ensure_accessibility_safe()?;
        // SAFETY: `application` is retained and `pid` is writable.
        if unsafe { AXUIElementGetPid(application.0, &mut pid) } != AX_ERROR_SUCCESS {
            return Err(InlineError::Accessibility);
        }
        let target = Self {
            refs: Some(TargetRefs {
                application,
                window,
                element,
                pid,
            }),
        };
        target.ensure_target_safe(&mut string_budget)?;
        Ok(target)
    }

    fn refs(&self) -> Result<&TargetRefs, InlineError> {
        self.refs.as_ref().ok_or(InlineError::Released)
    }

    fn ensure_target_safe(&self, string_budget: &mut AxStringBudget) -> Result<(), InlineError> {
        ensure_accessibility_safe()?;
        let refs = self.refs()?;
        let role = copy_text(refs.element.0, "AXRole", string_budget)?
            .ok_or(InlineError::Accessibility)?;
        let subrole = copy_text(refs.element.0, "AXSubrole", string_budget)?;
        let protected = copy_bool(refs.element.0, "AXProtectedContent")?.unwrap_or(false)
            || role_is_sensitive(&role, subrole.as_deref());
        if protected {
            Err(InlineError::ProtectedContent)
        } else {
            Ok(())
        }
    }

    fn current_metadata(
        &self,
        string_budget: &mut AxStringBudget,
    ) -> Result<FocusedElementMetadata, InlineError> {
        self.ensure_target_safe(string_budget)?;
        let refs = self.refs()?;
        let role = copy_text(refs.element.0, "AXRole", string_budget)?
            .ok_or(InlineError::Accessibility)?;
        let subrole = copy_text(refs.element.0, "AXSubrole", string_budget)?;
        let identifier = copy_text(refs.element.0, "AXIdentifier", string_budget)?;
        let title = copy_text(refs.element.0, "AXTitle", string_budget)?;
        let description = copy_text(refs.element.0, "AXDescription", string_budget)?;
        let selection = copy_range(refs.element.0, "AXSelectedTextRange")?;
        let selected_text_writable = is_settable(refs.element.0, "AXSelectedText")?;
        let value_writable = is_settable(refs.element.0, "AXValue")?;
        let contenteditable = role_is_contenteditable(
            &role,
            subrole.as_deref(),
            copy_bool(refs.element.0, "AXEditable")? == Some(true),
        );
        let window_title = match refs.window.as_ref() {
            Some(window) => copy_text(window.0, "AXTitle", string_budget)?,
            None => None,
        };
        let window_id = match refs.window.as_ref() {
            Some(window) => copy_positive_i64(window.0, "AXWindowNumber")?,
            None => None,
        };
        Ok(FocusedElementMetadata {
            pid: refs.pid,
            bundle_id: running_application_bundle_id(refs.pid, string_budget)?,
            window_title,
            window_id,
            role,
            subrole,
            title,
            description,
            identifier,
            frame: copy_frame(refs.element.0)?,
            selection,
            selected_text_writable,
            value_writable,
            contenteditable,
        })
    }

    fn current_read(
        &self,
        scope: TextScope,
        string_budget: &mut AxStringBudget,
    ) -> Result<InlineRead, InlineError> {
        let metadata = self.current_metadata(string_budget)?;
        let refs = self.refs()?;
        let text = match scope {
            TextScope::Selection => {
                if let Some(selected) = copy_text(refs.element.0, "AXSelectedText", string_budget)?
                {
                    selected
                } else {
                    let value = copy_text(refs.element.0, "AXValue", string_budget)?
                        .ok_or(InlineError::TextUnavailable)?;
                    let range = metadata.selection.ok_or(InlineError::TextUnavailable)?;
                    slice_utf16_range(&value, range)?
                }
            }
            TextScope::WholeDraft => copy_text(refs.element.0, "AXValue", string_budget)?
                .ok_or(InlineError::TextUnavailable)?,
        };
        Ok(InlineRead {
            scope,
            text,
            selection: metadata.selection,
            metadata,
        })
    }

    fn ensure_focus(&self, focus: DeliveryFocus) -> Result<(), InlineError> {
        ensure_accessibility_safe()?;
        let refs = self.refs()?;
        let system = OwnedAx::new(unsafe { AXUIElementCreateSystemWide() as CFTypeRef })
            .ok_or(InlineError::TargetFocusChanged)?;
        let application = copy_element(system.0, "AXFocusedApplication")?
            .ok_or(InlineError::TargetFocusChanged)?;
        let element = copy_element(application.0, "AXFocusedUIElement")?
            .ok_or(InlineError::TargetFocusChanged)?;
        ensure_accessibility_safe()?;
        let mut pid = 0;
        if unsafe { AXUIElementGetPid(application.0, &mut pid) } != AX_ERROR_SUCCESS {
            return Err(InlineError::TargetFocusChanged);
        }
        let target_focused = pid == refs.pid && unsafe { CFEqual(element.0, refs.element.0) };
        let allowed = match focus {
            DeliveryFocus::Target => target_focused,
            DeliveryFocus::ControllerOrTarget { controller_pid } => {
                target_focused
                    || (controller_pid > 0 && controller_pid != refs.pid && pid == controller_pid)
            }
        };
        if allowed {
            Ok(())
        } else {
            Err(InlineError::TargetFocusChanged)
        }
    }

    fn ensure_controller_focus(&self, controller_pid: i32) -> Result<(), InlineError> {
        ensure_accessibility_safe()?;
        let refs = self.refs()?;
        if controller_pid <= 0 || controller_pid == refs.pid {
            return Err(InlineError::TargetFocusChanged);
        }

        let system = OwnedAx::new(unsafe { AXUIElementCreateSystemWide() as CFTypeRef })
            .ok_or(InlineError::TargetFocusChanged)?;
        let application = copy_element(system.0, "AXFocusedApplication")?
            .ok_or(InlineError::TargetFocusChanged)?;
        let mut pid = 0;
        // SAFETY: `application` is retained and `pid` is writable.
        if unsafe { AXUIElementGetPid(application.0, &mut pid) } != AX_ERROR_SUCCESS
            || pid != controller_pid
        {
            return Err(InlineError::TargetFocusChanged);
        }
        Ok(())
    }

    fn validate_revision(
        &self,
        expected: &InlineRead,
        focus: DeliveryFocus,
        string_budget: &mut AxStringBudget,
    ) -> Result<(), InlineError> {
        self.ensure_target_safe(string_budget)?;
        self.ensure_focus(focus)?;
        let refs = self.refs()?;
        if expected.metadata.pid != refs.pid
            || expected.selection != expected.metadata.selection
            || self.current_read(expected.scope, string_budget)? != *expected
        {
            return Err(InlineError::TargetContentChanged);
        }
        Ok(())
    }

    fn wake_for_delivery(
        &mut self,
        expected: &InlineRead,
        focus: DeliveryFocus,
        string_budget: &mut AxStringBudget,
    ) -> Result<(), InlineError> {
        self.validate_revision(expected, focus, string_budget)?;
        if self.ensure_focus(DeliveryFocus::Target).is_ok() {
            return Ok(());
        }
        if !matches!(focus, DeliveryFocus::ControllerOrTarget { .. }) {
            return Err(InlineError::TargetFocusChanged);
        }

        // The controller was validated immediately above. A single bounded
        // focus restoration is permitted; an unrelated foreground process is
        // never used as authority to force this retained target.
        let refs = self.refs()?;
        if is_settable(refs.application.0, "AXFrontmost")? {
            set_attribute(refs.application.0, "AXFrontmost", unsafe { kCFBooleanTrue })?;
        }
        if let Some(window) = refs.window.as_ref() {
            let _ = perform_action(window.0, "AXRaise");
        }
        if is_settable(refs.element.0, "AXFocused")? {
            set_attribute(refs.element.0, "AXFocused", unsafe { kCFBooleanTrue })?;
        }

        for attempt in 0..6 {
            match self.validate_revision(expected, DeliveryFocus::Target, string_budget) {
                Ok(()) => return Ok(()),
                Err(InlineError::TargetFocusChanged) if attempt < 5 => {
                    thread::sleep(Duration::from_millis(12));
                }
                Err(error) => return Err(error),
            }
        }
        Err(InlineError::TargetFocusChanged)
    }

    fn fallback_range(expected: &InlineRead) -> Result<Utf16Range, InlineError> {
        match expected.scope {
            TextScope::Selection => expected.selection.ok_or(InlineError::TextUnavailable),
            TextScope::WholeDraft => Ok(Utf16Range {
                location: 0,
                length: expected.text.encode_utf16().count(),
            }),
        }
    }

    fn validate_prepared_fallback(
        &self,
        expected: &InlineRead,
        fallback: FallbackTarget,
        string_budget: &mut AxStringBudget,
    ) -> Result<(), InlineError> {
        self.ensure_target_safe(string_budget)?;
        self.ensure_focus(DeliveryFocus::Target)?;
        let refs = self.refs()?;
        if fallback.pid != refs.pid || fallback.selection != Self::fallback_range(expected)? {
            return Err(InlineError::TargetContentChanged);
        }
        let mut revised_expected = expected.clone();
        revised_expected.selection = Some(fallback.selection);
        revised_expected.metadata.selection = Some(fallback.selection);
        if self.current_read(expected.scope, string_budget)? != revised_expected {
            return Err(InlineError::TargetContentChanged);
        }
        Ok(())
    }

    fn set_text(&self, attribute: &str, value: &str) -> Result<(), InlineError> {
        let refs = self.refs()?;
        let value = create_cf_string(value).ok_or(InlineError::Accessibility)?;
        set_attribute(refs.element.0, attribute, value.0)
    }

    fn set_whole_draft_accessibility(
        &self,
        expected: &InlineRead,
        replacement: &str,
    ) -> Result<(), InlineError> {
        if expected.scope != TextScope::WholeDraft {
            return Err(InlineError::NotWritable);
        }
        let refs = self.refs()?;
        if expected.metadata.value_writable && is_settable(refs.element.0, "AXValue")? {
            self.set_text("AXValue", replacement)?;
        } else if expected.metadata.selected_text_writable
            && is_settable(refs.element.0, "AXSelectedTextRange")?
        {
            set_range(
                refs.element.0,
                "AXSelectedTextRange",
                Utf16Range {
                    location: 0,
                    length: expected.text.encode_utf16().count(),
                },
            )?;
            self.set_text("AXSelectedText", replacement)?;
        } else {
            return Err(InlineError::NotWritable);
        }

        if is_settable(refs.element.0, "AXSelectedTextRange")? {
            let _ = set_range(
                refs.element.0,
                "AXSelectedTextRange",
                Utf16Range {
                    location: replacement.encode_utf16().count(),
                    length: 0,
                },
            );
        }
        Ok(())
    }

    fn ensure_whole_draft_accessibility_writable(
        &self,
        expected: &InlineRead,
    ) -> Result<(), InlineError> {
        if expected.scope != TextScope::WholeDraft {
            return Err(InlineError::NotWritable);
        }
        let refs = self.refs()?;
        if expected.metadata.value_writable && is_settable(refs.element.0, "AXValue")? {
            return Ok(());
        }
        if expected.metadata.selected_text_writable
            && is_settable(refs.element.0, "AXSelectedText")?
            && is_settable(refs.element.0, "AXSelectedTextRange")?
        {
            return Ok(());
        }
        Err(InlineError::NotWritable)
    }

    fn confirmed_whole_draft_read(&self, replacement: &str) -> Result<InlineRead, InlineError> {
        for attempt in 0..DELIVERY_CONFIRMATION_ATTEMPTS {
            let mut string_budget = AxStringBudget::default();
            match self.current_read(TextScope::WholeDraft, &mut string_budget) {
                Ok(observed) if observed.text == replacement => return Ok(observed),
                Err(
                    error @ (InlineError::PermissionDenied
                    | InlineError::SecureInput
                    | InlineError::ProtectedContent
                    | InlineError::Released),
                ) => return Err(error),
                Ok(_) | Err(_) => {}
            }
            if attempt + 1 < DELIVERY_CONFIRMATION_ATTEMPTS {
                thread::sleep(DELIVERY_CONFIRMATION_DELAY);
            }
        }
        Err(InlineError::DeliveryUnconfirmed)
    }

    fn replace_value_range(
        &self,
        expected_value: &str,
        range: Utf16Range,
        replacement: &str,
        string_budget: &mut AxStringBudget,
    ) -> Result<ReplacementAttempt, InlineError> {
        let refs = self.refs()?;
        let value = copy_text(refs.element.0, "AXValue", string_budget)?
            .ok_or(InlineError::TextUnavailable)?;
        if value != expected_value {
            return Err(InlineError::TargetContentChanged);
        }
        let updated = replace_utf16_range(&value, range, replacement)?;
        self.set_text("AXValue", &updated)?;
        let caret = Utf16Range {
            location: range
                .location
                .checked_add(replacement.encode_utf16().count())
                .ok_or(InlineError::InvalidRange)?,
            length: 0,
        };
        if is_settable(refs.element.0, "AXSelectedTextRange")? {
            let _ = set_range(refs.element.0, "AXSelectedTextRange", caret);
        }
        Ok(ReplacementAttempt::ValueRange)
    }
}

impl FocusedTextTarget for MacOsFocusedTarget {
    fn metadata(&self) -> Result<FocusedElementMetadata, InlineError> {
        let mut string_budget = AxStringBudget::default();
        self.current_metadata(&mut string_budget)
    }

    fn read(&self, scope: TextScope) -> Result<InlineRead, InlineError> {
        let mut string_budget = AxStringBudget::default();
        self.current_read(scope, &mut string_budget)
    }

    fn validate(&self, expected: &InlineRead, focus: DeliveryFocus) -> Result<(), InlineError> {
        let mut string_budget = AxStringBudget::default();
        self.validate_revision(expected, focus, &mut string_budget)
    }

    fn validate_controller_focus(&self, controller_pid: i32) -> Result<(), InlineError> {
        self.ensure_controller_focus(controller_pid)
    }

    fn replace(
        &mut self,
        expected: &InlineRead,
        replacement: &str,
        focus: DeliveryFocus,
    ) -> Result<ReplacementAttempt, InlineError> {
        let mut string_budget = AxStringBudget::default();
        // A verified rewrite controller may authorize exactly one restoration
        // of the retained target. Every AX text mutation itself requires that
        // target to be the exact system-wide focused element.
        self.wake_for_delivery(expected, focus, &mut string_budget)?;
        self.validate_revision(expected, DeliveryFocus::Target, &mut string_budget)?;
        match expected.scope {
            TextScope::Selection if expected.metadata.selected_text_writable => {
                self.validate_revision(expected, DeliveryFocus::Target, &mut string_budget)?;
                self.set_text("AXSelectedText", replacement)?;
                Ok(ReplacementAttempt::SelectedText)
            }
            TextScope::Selection if expected.metadata.value_writable => {
                let range = expected.selection.ok_or(InlineError::TextUnavailable)?;
                let refs = self.refs()?;
                let expected_value = copy_text(refs.element.0, "AXValue", &mut string_budget)?
                    .ok_or(InlineError::TextUnavailable)?;
                self.validate_revision(expected, DeliveryFocus::Target, &mut string_budget)?;
                self.replace_value_range(&expected_value, range, replacement, &mut string_budget)
            }
            TextScope::WholeDraft if expected.metadata.value_writable => {
                self.validate_revision(expected, DeliveryFocus::Target, &mut string_budget)?;
                self.set_text("AXValue", replacement)?;
                Ok(ReplacementAttempt::Value)
            }
            _ => Ok(ReplacementAttempt::Unavailable),
        }
    }

    fn replace_whole_draft_preview(
        &mut self,
        expected_text: &str,
        replacement: &str,
        controller_pid: i32,
    ) -> Result<InlineRead, PreviewWriteError> {
        self.ensure_controller_focus(controller_pid)
            .map_err(PreviewWriteError::before_write)?;
        let mut string_budget = AxStringBudget::default();
        let current = self
            .current_read(TextScope::WholeDraft, &mut string_budget)
            .map_err(PreviewWriteError::before_write)?;
        if current.text != expected_text {
            return Err(PreviewWriteError::before_write(
                InlineError::TargetContentChanged,
            ));
        }

        // Keep the temporary text unreachable by foreground keyboard input
        // through the write itself. The retained element is mutated through
        // Accessibility only; preview updates never use clipboard or injected
        // key events and never raise the target application.
        self.ensure_controller_focus(controller_pid)
            .map_err(PreviewWriteError::before_write)?;
        self.ensure_whole_draft_accessibility_writable(&current)
            .map_err(PreviewWriteError::before_write)?;
        self.set_whole_draft_accessibility(&current, replacement)
            .map_err(PreviewWriteError::after_write_started)?;
        self.confirmed_whole_draft_read(replacement)
            .map_err(PreviewWriteError::after_write_started)
    }

    fn restore_whole_draft_preview(
        &mut self,
        expected_previews: &[&str],
        original: &InlineRead,
        controller_pid: i32,
    ) -> Result<InlineRead, InlineError> {
        if original.scope != TextScope::WholeDraft {
            return Err(InlineError::NotWritable);
        }
        let mut string_budget = AxStringBudget::default();
        self.ensure_target_safe(&mut string_budget)?;
        let current = self.current_read(TextScope::WholeDraft, &mut string_budget)?;
        if current.text == original.text {
            return Ok(current);
        }
        if !expected_previews
            .iter()
            .any(|preview| current.text == *preview)
        {
            return Err(InlineError::TargetContentChanged);
        }

        // Cleanup mutates the composer only while woof's controller still
        // owns foreground keyboard focus. If the user has returned to the
        // composer, leaving a marker is safer than racing their keystrokes.
        self.ensure_controller_focus(controller_pid)?;
        let mut recheck_budget = AxStringBudget::default();
        let current = self.current_read(TextScope::WholeDraft, &mut recheck_budget)?;
        if current.text == original.text {
            return Ok(current);
        }
        if !expected_previews
            .iter()
            .any(|preview| current.text == *preview)
        {
            return Err(InlineError::TargetContentChanged);
        }
        self.ensure_controller_focus(controller_pid)?;
        self.set_whole_draft_accessibility(&current, &original.text)?;
        self.confirmed_whole_draft_read(&original.text)?;

        if let Some(selection) = original.selection {
            let refs = self.refs()?;
            if is_settable(refs.element.0, "AXSelectedTextRange")? {
                set_range(refs.element.0, "AXSelectedTextRange", selection)?;
            }
        }

        let mut final_budget = AxStringBudget::default();
        let restored = self.current_read(TextScope::WholeDraft, &mut final_budget)?;
        if restored.text != original.text {
            return Err(InlineError::DeliveryUnconfirmed);
        }
        Ok(restored)
    }

    fn prepare_fallback(
        &mut self,
        expected: &InlineRead,
        _hint: WakeHint,
        focus: DeliveryFocus,
    ) -> Result<FallbackTarget, InlineError> {
        let mut string_budget = AxStringBudget::default();
        self.wake_for_delivery(expected, focus, &mut string_budget)?;
        // Require the exact retained AX element after focus restoration and
        // before selecting the captured UTF-16 revision range.
        self.validate_revision(expected, DeliveryFocus::Target, &mut string_budget)?;
        let refs = self.refs()?;
        let selection = Self::fallback_range(expected)?;
        if expected.selection != Some(selection) {
            if !is_settable(refs.element.0, "AXSelectedTextRange")? {
                return Err(InlineError::NotWritable);
            }
            self.validate_revision(expected, DeliveryFocus::Target, &mut string_budget)?;
            set_range(refs.element.0, "AXSelectedTextRange", selection)?;
        }
        let fallback = FallbackTarget {
            pid: refs.pid,
            selection,
        };
        self.validate_prepared_fallback(expected, fallback, &mut string_budget)?;
        Ok(fallback)
    }

    fn validate_fallback(
        &self,
        expected: &InlineRead,
        fallback: FallbackTarget,
    ) -> Result<(), InlineError> {
        let mut string_budget = AxStringBudget::default();
        self.validate_prepared_fallback(expected, fallback, &mut string_budget)
    }

    fn confirm_whole_draft(&self, replacement: &str) -> Result<(), InlineError> {
        self.confirmed_whole_draft_read(replacement).map(|_| ())
    }

    fn release(&mut self) {
        self.refs = None;
    }
}

impl Drop for MacOsFocusedTarget {
    fn drop(&mut self) {
        self.refs = None;
    }
}

fn ensure_accessibility_safe() -> Result<(), InlineError> {
    // SAFETY: Both functions read process-global security state.
    unsafe {
        if !AXIsProcessTrusted() {
            return Err(InlineError::PermissionDenied);
        }
        if IsSecureEventInputEnabled() {
            return Err(InlineError::SecureInput);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct AxStringBudget {
    remaining_bytes: usize,
}

impl AxStringBudget {
    #[cfg(test)]
    fn new(maximum_bytes: usize) -> Self {
        Self {
            remaining_bytes: maximum_bytes,
        }
    }

    /// Reserve every successful conversion for the lifetime of one public AX
    /// operation. Reservations are intentionally not returned when a string is
    /// dropped: revision checks and focus-restoration retries must share the
    /// same cumulative allocation ceiling.
    fn with_conversion_capacity<T>(
        &mut self,
        requested_capacity: usize,
        convert: impl FnOnce(usize) -> Option<T>,
    ) -> Result<Option<T>, InlineError> {
        debug_assert!(requested_capacity > 0);
        let reserved_capacity = requested_capacity.min(self.remaining_bytes);
        if reserved_capacity == 0 {
            return Err(InlineError::Accessibility);
        }
        self.remaining_bytes -= reserved_capacity;
        if let Some(value) = convert(reserved_capacity) {
            return Ok(Some(value));
        }

        self.remaining_bytes += reserved_capacity;
        if reserved_capacity < requested_capacity {
            Err(InlineError::Accessibility)
        } else {
            // Preserve the previous whole-field skip for a value that cannot
            // be converted within its independent per-field capacity.
            Ok(None)
        }
    }

    #[cfg(test)]
    fn remaining_bytes(&self) -> usize {
        self.remaining_bytes
    }
}

impl Default for AxStringBudget {
    fn default() -> Self {
        Self {
            remaining_bytes: MAX_OPERATION_STRING_ALLOCATION_BYTES,
        }
    }
}

fn copy_element(element: AXUIElementRef, attribute: &str) -> Result<Option<OwnedAx>, InlineError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    if unsafe { CFGetTypeID(value.0) } != unsafe { AXUIElementGetTypeID() } {
        return Ok(None);
    }
    Ok(OwnedAx::new(value.into_raw()))
}

fn copy_text(
    element: AXUIElementRef,
    attribute: &str,
    string_budget: &mut AxStringBudget,
) -> Result<Option<String>, InlineError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    // SAFETY: Type IDs are checked before conversion and `value` remains
    // retained for the entire conversion.
    unsafe {
        let type_id = CFGetTypeID(value.0);
        if type_id == CFStringGetTypeID() {
            cf_string_to_rust(value.0 as CFStringRef, string_budget)
        } else if type_id == CFAttributedStringGetTypeID() {
            cf_string_to_rust(
                CFAttributedStringGetString(value.0 as CFAttributedStringRef),
                string_budget,
            )
        } else if type_id == CFURLGetTypeID() {
            cf_string_to_rust(CFURLGetString(value.0 as CFURLRef), string_budget)
        } else {
            Ok(None)
        }
    }
}

fn copy_bool(element: AXUIElementRef, attribute: &str) -> Result<Option<bool>, InlineError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    // SAFETY: Type is checked before the CFBoolean read.
    unsafe {
        Ok((CFGetTypeID(value.0) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(value.0)))
    }
}

fn copy_positive_i64(element: AXUIElementRef, attribute: &str) -> Result<Option<i64>, InlineError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    // SAFETY: Type identity is checked before CFNumber copies a signed 64-bit
    // value into correctly aligned Rust storage. AX window numbers are valid
    // identities only when strictly positive.
    unsafe {
        if CFGetTypeID(value.0) != CFNumberGetTypeID() {
            return Ok(None);
        }
        let mut output = 0_i64;
        if !CFNumberGetValue(
            value.0 as CFNumberRef,
            CF_NUMBER_SINT64_TYPE,
            ptr::addr_of_mut!(output).cast(),
        ) {
            return Ok(None);
        }
        Ok((output > 0).then_some(output))
    }
}

fn copy_range(element: AXUIElementRef, attribute: &str) -> Result<Option<Utf16Range>, InlineError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    // SAFETY: AXValue type is checked before copying the CFRange payload.
    unsafe {
        if CFGetTypeID(value.0) != AXValueGetTypeID()
            || AXValueGetType(value.0 as AXValueRef) != AX_VALUE_CFRANGE
        {
            return Ok(None);
        }
        let mut range = CFRange::default();
        if !AXValueGetValue(
            value.0 as AXValueRef,
            AX_VALUE_CFRANGE,
            ptr::addr_of_mut!(range).cast(),
        ) {
            return Ok(None);
        }
        let location = usize::try_from(range.location).ok();
        let length = usize::try_from(range.length).ok();
        Ok(location
            .zip(length)
            .map(|(location, length)| Utf16Range { location, length }))
    }
}

fn copy_frame(element: AXUIElementRef) -> Result<Option<Rect>, InlineError> {
    if let Some(frame) = copy_ax_value::<CGRect>(element, "AXFrame", AX_VALUE_CGRECT)? {
        return Ok(Some(Rect {
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        }));
    }
    let position = copy_ax_value::<CGPoint>(element, "AXPosition", AX_VALUE_CGPOINT)?;
    let size = copy_ax_value::<CGSize>(element, "AXSize", AX_VALUE_CGSIZE)?;
    Ok(position.zip(size).map(|(position, size)| Rect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    }))
}

fn copy_ax_value<T: Default>(
    element: AXUIElementRef,
    attribute: &str,
    value_type: i32,
) -> Result<Option<T>, InlineError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    // SAFETY: The requested type is checked before copying into a correctly
    // sized output value.
    unsafe {
        if CFGetTypeID(value.0) != AXValueGetTypeID()
            || AXValueGetType(value.0 as AXValueRef) != value_type
        {
            return Ok(None);
        }
        let mut output = T::default();
        if AXValueGetValue(
            value.0 as AXValueRef,
            value_type,
            ptr::addr_of_mut!(output).cast(),
        ) {
            Ok(Some(output))
        } else {
            Ok(None)
        }
    }
}

fn copy_attribute(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<Option<OwnedCf>, InlineError> {
    ensure_accessibility_safe()?;
    let attribute = create_cf_string(attribute).ok_or(InlineError::Accessibility)?;
    let mut value = ptr::null();
    // SAFETY: Element and attribute are retained; the out pointer is valid.
    let error = unsafe { AXUIElementCopyAttributeValue(element, attribute.0, &mut value) };
    match error {
        AX_ERROR_SUCCESS => Ok(OwnedCf::new(value)),
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => Ok(None),
        AX_ERROR_API_DISABLED => Err(InlineError::PermissionDenied),
        AX_ERROR_INVALID_UI_ELEMENT | AX_ERROR_CANNOT_COMPLETE => Err(InlineError::Accessibility),
        _ => Err(InlineError::Accessibility),
    }
}

fn set_attribute(
    element: AXUIElementRef,
    attribute: &str,
    value: CFTypeRef,
) -> Result<(), InlineError> {
    ensure_accessibility_safe()?;
    let attribute = create_cf_string(attribute).ok_or(InlineError::Accessibility)?;
    // SAFETY: Element, attribute, and value remain valid for the call.
    if unsafe { AXUIElementSetAttributeValue(element, attribute.0, value) } == AX_ERROR_SUCCESS {
        Ok(())
    } else {
        Err(InlineError::Accessibility)
    }
}

fn is_settable(element: AXUIElementRef, attribute: &str) -> Result<bool, InlineError> {
    ensure_accessibility_safe()?;
    let attribute = create_cf_string(attribute).ok_or(InlineError::Accessibility)?;
    let mut settable = false;
    // SAFETY: The element and attribute are retained and out pointer valid.
    let error = unsafe { AXUIElementIsAttributeSettable(element, attribute.0, &mut settable) };
    match error {
        AX_ERROR_SUCCESS => Ok(settable),
        AX_ERROR_ATTRIBUTE_UNSUPPORTED | AX_ERROR_NO_VALUE => Ok(false),
        AX_ERROR_API_DISABLED => Err(InlineError::PermissionDenied),
        _ => Err(InlineError::Accessibility),
    }
}

fn set_range(
    element: AXUIElementRef,
    attribute: &str,
    range: Utf16Range,
) -> Result<(), InlineError> {
    let native = CFRange {
        location: range
            .location
            .try_into()
            .map_err(|_| InlineError::InvalidRange)?,
        length: range
            .length
            .try_into()
            .map_err(|_| InlineError::InvalidRange)?,
    };
    // SAFETY: `native` matches the AXValue CFRange layout.
    let value =
        OwnedCf::new(unsafe { AXValueCreate(AX_VALUE_CFRANGE, ptr::addr_of!(native).cast()) })
            .ok_or(InlineError::Accessibility)?;
    set_attribute(element, attribute, value.0)
}

fn perform_action(element: AXUIElementRef, action: &str) -> Result<(), InlineError> {
    ensure_accessibility_safe()?;
    let action = create_cf_string(action).ok_or(InlineError::Accessibility)?;
    // SAFETY: Element and action remain valid for the call.
    if unsafe { AXUIElementPerformAction(element, action.0) } == AX_ERROR_SUCCESS {
        Ok(())
    } else {
        Err(InlineError::Accessibility)
    }
}

fn create_cf_string(value: &str) -> Option<OwnedCf> {
    let length: CFIndex = value.len().try_into().ok()?;
    // SAFETY: The byte pointer and length describe `value` for this call.
    OwnedCf::new(unsafe {
        CFStringCreateWithBytes(ptr::null(), value.as_ptr(), length, UTF8, false)
    })
}

unsafe fn cf_string_to_rust(
    value: CFStringRef,
    string_budget: &mut AxStringBudget,
) -> Result<Option<String>, InlineError> {
    if value.is_null() {
        return Ok(None);
    }
    let length = CFStringGetLength(value);
    let maximum = CFStringGetMaximumSizeForEncoding(length, UTF8)
        .saturating_add(1)
        .max(1) as usize;
    if maximum > MAX_TEXT_BYTES.saturating_add(1) {
        return Ok(None);
    }
    string_budget.with_conversion_capacity(maximum, |capacity| {
        let mut buffer = vec![0_u8; capacity];
        if !CFStringGetCString(
            value,
            buffer.as_mut_ptr().cast(),
            buffer.len() as CFIndex,
            UTF8,
        ) {
            zeroize_bytes(&mut buffer);
            return None;
        }
        nul_terminated_utf8_buffer_into_string(buffer)
    })
}

fn nul_terminated_utf8_buffer_into_string(mut buffer: Vec<u8>) -> Option<String> {
    let length = buffer
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(buffer.len());
    zeroize_bytes(&mut buffer[length..]);
    buffer.truncate(length);
    match String::from_utf8(buffer) {
        Ok(value) => Some(value),
        Err(error) => {
            let mut buffer = error.into_bytes();
            zeroize_bytes(&mut buffer);
            None
        }
    }
}

fn zeroize_bytes(buffer: &mut [u8]) {
    for byte in buffer {
        // SAFETY: `byte` is a valid unique pointer for the duration of this
        // iteration. Volatile writes prevent elision of sensitive cleanup.
        unsafe { ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

fn running_application_bundle_id(
    pid: i32,
    string_budget: &mut AxStringBudget,
) -> Result<Option<String>, InlineError> {
    autoreleasepool(|_| {
        let Some(class) = AnyClass::get(c"NSRunningApplication") else {
            return Ok(None);
        };
        // SAFETY: Selectors and return types match NSRunningApplication APIs.
        // The method-family-none results remain valid until this synchronous
        // conversion finishes, then the local pool drains them before return.
        unsafe {
            let application: *mut AnyObject =
                msg_send![class, runningApplicationWithProcessIdentifier: pid];
            let Some(application) = application.as_ref() else {
                return Ok(None);
            };
            let bundle: *mut NSString = msg_send![application, bundleIdentifier];
            cf_string_to_rust(bundle.cast::<c_void>(), string_budget)
        }
    })
}

fn role_is_sensitive(role: &str, subrole: Option<&str>) -> bool {
    let combined = format!("{role} {}", subrole.unwrap_or_default()).to_ascii_lowercase();
    combined.contains("securetextfield")
        || combined.contains("password")
        || combined.contains("secure text")
}

fn role_is_contenteditable(role: &str, subrole: Option<&str>, editable: bool) -> bool {
    editable
        && (matches!(role, "AXTextArea" | "AXTextField")
            || matches!(subrole, Some("AXTextEntryArea")))
}

struct OwnedCf(CFTypeRef);

impl OwnedCf {
    fn new(value: CFTypeRef) -> Option<Self> {
        (!value.is_null()).then_some(Self(value))
    }

    fn into_raw(self) -> CFTypeRef {
        let value = self.0;
        std::mem::forget(self);
        value
    }
}

impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: OwnedCf wraps only create/copy-rule values.
        unsafe { CFRelease(self.0) };
    }
}

/// Retained AXUIElement handle. Accessibility element APIs may be called from
/// a worker thread; woof only moves each session-owned handle and never shares
/// it concurrently, so this wrapper deliberately implements `Send` only.
struct OwnedAx(CFTypeRef);

impl OwnedAx {
    fn new(value: CFTypeRef) -> Option<Self> {
        (!value.is_null()).then_some(Self(value))
    }
}

impl Drop for OwnedAx {
    fn drop(&mut self) {
        // SAFETY: OwnedAx wraps only create/copy-rule AX element values.
        unsafe { CFRelease(self.0) };
    }
}

unsafe impl Send for OwnedAx {}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn aggregate_string_budget_caps_cumulative_successful_conversions() {
        let mut budget = AxStringBudget::new(8);

        for _ in 0..2 {
            assert_eq!(
                budget
                    .with_conversion_capacity(4, |_| Some(()))
                    .expect("conversion within budget"),
                Some(())
            );
        }
        let conversions = Cell::new(0);
        assert_eq!(
            budget.with_conversion_capacity(1, |_| {
                conversions.set(conversions.get() + 1);
                Some(())
            }),
            Err(InlineError::Accessibility)
        );

        assert_eq!(conversions.get(), 0);
        assert_eq!(budget.remaining_bytes(), 0);
    }

    #[test]
    fn aggregate_string_budget_rejects_a_partial_field_and_refunds_it() {
        let mut budget = AxStringBudget::new(4);
        let observed_capacity = Cell::new(0);

        let result: Result<Option<String>, InlineError> =
            budget.with_conversion_capacity(8, |capacity| {
                observed_capacity.set(capacity);
                None
            });

        assert_eq!(result, Err(InlineError::Accessibility));
        assert_eq!(observed_capacity.get(), 4);
        assert_eq!(budget.remaining_bytes(), 4);
    }

    #[test]
    fn independent_field_conversion_failure_remains_a_whole_field_skip() {
        let mut budget = AxStringBudget::new(8);

        let result: Result<Option<String>, InlineError> =
            budget.with_conversion_capacity(8, |_| None);

        assert_eq!(result, Ok(None));
        assert_eq!(budget.remaining_bytes(), 8);
    }

    #[test]
    fn conversion_reuses_a_complete_multibyte_buffer_and_wipes_unused_bytes() {
        let source = "Grüße";
        let mut buffer = Vec::with_capacity(32);
        buffer.extend_from_slice(source.as_bytes());
        buffer.push(0);
        buffer.resize(32, 0x7f);

        let converted =
            nul_terminated_utf8_buffer_into_string(buffer).expect("valid complete UTF-8");

        assert_eq!(converted, source);
        assert_eq!(converted.capacity(), 32);
    }

    #[test]
    fn explicit_buffer_cleanup_overwrites_every_byte() {
        let mut buffer = [0x7f; 32];

        zeroize_bytes(&mut buffer);

        assert_eq!(buffer, [0; 32]);
    }

    #[test]
    fn invalid_utf8_conversion_is_rejected_whole() {
        let buffer = vec![0xff, 0];

        assert!(nul_terminated_utf8_buffer_into_string(buffer).is_none());
    }

    #[test]
    fn contenteditable_requires_a_real_editable_ax_text_role() {
        assert!(role_is_contenteditable("AXTextArea", None, true));
        assert!(role_is_contenteditable(
            "AXGroup",
            Some("AXTextEntryArea"),
            true
        ));
        assert!(!role_is_contenteditable("AXButton", None, true));
        assert!(!role_is_contenteditable("AXTextArea", None, false));
    }
}
