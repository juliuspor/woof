use std::{ffi::c_void, thread, time::Duration};

use crate::{utf16_chunks, InlineError, InputInjector};

type CFTypeRef = *const c_void;
type CGEventSourceRef = *mut c_void;
type CGEventRef = *mut c_void;

const EVENT_SOURCE_COMBINED_SESSION: i32 = 0;
const COMMAND_FLAG: u64 = 1 << 20;
const KEY_V: u16 = 9;
const KEY_DELETE: u16 = 51;
const UNICODE_CHUNK_UNITS: usize = 20;
const KEY_GAP: Duration = Duration::from_millis(6);
const PASTE_SETTLE: Duration = Duration::from_millis(90);

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        virtual_key: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventKeyboardSetUnicodeString(
        event: CGEventRef,
        string_length: usize,
        unicode_string: *const u16,
    );
    fn CGEventPostToPid(pid: i32, event: CGEventRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: CFTypeRef);
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MacOsInputInjector;

impl MacOsInputInjector {
    fn ensure_safe() -> Result<(), InlineError> {
        // SAFETY: This Carbon function reads process-global secure-input state.
        if unsafe { IsSecureEventInputEnabled() } {
            Err(InlineError::SecureInput)
        } else {
            Ok(())
        }
    }

    fn post_key(&self, pid: i32, key_code: u16, flags: u64) -> Result<(), InlineError> {
        Self::ensure_safe()?;
        let source = OwnedCf::new(unsafe { CGEventSourceCreate(EVENT_SOURCE_COMBINED_SESSION) })
            .ok_or(InlineError::InputInjection)?;
        let down = OwnedCf::new(unsafe { CGEventCreateKeyboardEvent(source.0, key_code, true) })
            .ok_or(InlineError::InputInjection)?;
        let up = OwnedCf::new(unsafe { CGEventCreateKeyboardEvent(source.0, key_code, false) })
            .ok_or(InlineError::InputInjection)?;
        Self::ensure_safe()?;
        // SAFETY: All event references are owned and valid for these calls.
        unsafe {
            CGEventSetFlags(down.0, flags);
            CGEventSetFlags(up.0, flags);
            CGEventPostToPid(pid, down.0);
        }
        let mut key_down = KeyDownGuard::new(pid, up);
        thread::sleep(KEY_GAP);
        // If secure input flips after key-down, dropping the guard emits only
        // the matching targeted key-up and no further synthetic input.
        Self::ensure_safe()?;
        key_down.release();
        thread::sleep(KEY_GAP);
        Ok(())
    }

    fn post_unicode_chunk(&self, pid: i32, units: &[u16]) -> Result<(), InlineError> {
        Self::ensure_safe()?;
        let source = OwnedCf::new(unsafe { CGEventSourceCreate(EVENT_SOURCE_COMBINED_SESSION) })
            .ok_or(InlineError::InputInjection)?;
        let down = OwnedCf::new(unsafe { CGEventCreateKeyboardEvent(source.0, 0, true) })
            .ok_or(InlineError::InputInjection)?;
        let up = OwnedCf::new(unsafe { CGEventCreateKeyboardEvent(source.0, 0, false) })
            .ok_or(InlineError::InputInjection)?;
        Self::ensure_safe()?;
        // SAFETY: `units` remains alive while Core Graphics copies it into
        // both keyboard events.
        unsafe {
            CGEventKeyboardSetUnicodeString(down.0, units.len(), units.as_ptr());
            CGEventKeyboardSetUnicodeString(up.0, units.len(), units.as_ptr());
            CGEventPostToPid(pid, down.0);
        }
        let mut key_down = KeyDownGuard::new(pid, up);
        thread::sleep(KEY_GAP);
        Self::ensure_safe()?;
        key_down.release();
        thread::sleep(KEY_GAP);
        Ok(())
    }
}

impl InputInjector for MacOsInputInjector {
    fn paste(&mut self, pid: i32) -> Result<(), InlineError> {
        self.post_key(pid, KEY_V, COMMAND_FLAG)?;
        thread::sleep(PASTE_SETTLE);
        Ok(())
    }

    fn type_unicode(&mut self, pid: i32, value: &str) -> Result<(), InlineError> {
        if value.is_empty() {
            return self.post_key(pid, KEY_DELETE, 0);
        }
        for chunk in utf16_chunks(value, UNICODE_CHUNK_UNITS)? {
            Self::ensure_safe()?;
            self.post_unicode_chunk(pid, &chunk)?;
        }
        Ok(())
    }
}

struct KeyDownGuard {
    pid: i32,
    key_up: Option<OwnedCf>,
}

impl KeyDownGuard {
    fn new(pid: i32, key_up: OwnedCf) -> Self {
        Self {
            pid,
            key_up: Some(key_up),
        }
    }

    fn release(&mut self) {
        self.post_key_up();
    }

    fn post_key_up(&mut self) {
        let Some(key_up) = self.key_up.take() else {
            return;
        };
        // SAFETY: This is the exact owned key-up paired with the targeted
        // key-down. It must be posted even when secure input has just flipped.
        unsafe { CGEventPostToPid(self.pid, key_up.0) };
    }
}

impl Drop for KeyDownGuard {
    fn drop(&mut self) {
        self.post_key_up();
    }
}

struct OwnedCf(*mut c_void);

impl OwnedCf {
    fn new(value: *mut c_void) -> Option<Self> {
        (!value.is_null()).then_some(Self(value))
    }
}

impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: Values are returned by Core Graphics create-rule functions.
        unsafe { CFRelease(self.0.cast()) };
    }
}
