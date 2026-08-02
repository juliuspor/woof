//! macOS Accessibility provider.
//!
//! The implementation uses the public AX/CoreFoundation APIs directly to keep
//! the capture crate free from Objective-C framework wrappers. Every copied
//! CoreFoundation object is released before returning.

use std::{
    cell::RefCell,
    ffi::{c_char, c_void, CString},
    ptr,
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use zeroize::{Zeroize, Zeroizing};

use crate::{
    capture_contextual_reply_after_surface_preflight, validate_capture_target, AccessibilityNode,
    AccessibilityProvider, AccessibilityRect, CaptureError, CaptureMetadata, CapturePolicy,
    ForegroundCapture, WOOF_BUNDLE_ID,
};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFNumberRef = *const c_void;
type CFArrayRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFTypeId = usize;
type CFIndex = isize;
type AXUIElementRef = *const c_void;
type AXValueRef = *const c_void;
type AXError = i32;
type OSErr = i16;
type OSStatus = i32;

const AX_ERROR_SUCCESS: AXError = 0;
const NO_ERR: OSStatus = 0;
const UTF8: u32 = 0x0800_0100;
const AX_VALUE_CGPOINT: i32 = 1;
const AX_VALUE_CGSIZE: i32 = 2;
const AX_VALUE_CGRECT: i32 = 3;
const CF_NUMBER_SINT64_TYPE: i32 = 4;
const MAX_CAPTURE_STRING_ALLOCATION_BYTES: usize = 4 * 1024 * 1024;
const CAPTURE_STRING_BUDGET_EXHAUSTED: &str = "capture string allocation budget exhausted";

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
struct ProcessSerialNumber {
    high_long_of_psn: u32,
    low_long_of_psn: u32,
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
    fn AXUIElementGetTypeID() -> CFTypeId;
    fn AXValueGetTypeID() -> CFTypeId;
    fn AXValueGetType(value: AXValueRef) -> i32;
    fn AXValueGetValue(value: AXValueRef, value_type: i32, output: *mut c_void) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: CFTypeRef);
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: CFIndex,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFGetTypeID(value: CFTypeRef) -> CFTypeId;
    fn CFEqual(left: CFTypeRef, right: CFTypeRef) -> bool;
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        value: *const c_char,
        encoding: u32,
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
    fn CFURLGetTypeID() -> CFTypeId;
    fn CFURLGetString(value: CFTypeRef) -> CFStringRef;
    fn CFArrayGetTypeID() -> CFTypeId;
    fn CFArrayGetCount(value: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(value: CFArrayRef, index: CFIndex) -> CFTypeRef;
    fn CFDictionaryGetValue(dictionary: CFDictionaryRef, key: CFTypeRef) -> CFTypeRef;
    fn CFBooleanGetTypeID() -> CFTypeId;
    fn CFBooleanGetValue(value: CFTypeRef) -> bool;
    fn CFNumberGetTypeID() -> CFTypeId;
    fn CFNumberGetValue(number: CFNumberRef, number_type: i32, value: *mut c_void) -> bool;
}

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetFrontProcess(process: *mut ProcessSerialNumber) -> OSErr;
    fn GetProcessPID(process: *const ProcessSerialNumber, pid: *mut i32) -> OSStatus;
    fn IsSecureEventInputEnabled() -> bool;
}

// Force-load AppKit so NSWorkspace and NSRunningApplication are registered in
// a standalone daemon process, not only when this crate is hosted by the Tauri
// UI.
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn objc_msgSend(receiver: *mut c_void, selector: *mut c_void, ...) -> *mut c_void;
}

extern "C" {
    fn getppid() -> i32;
}

#[derive(Clone, Debug)]
pub struct MacOsAccessibilityProvider {
    max_depth: usize,
    max_nodes: usize,
    max_field_bytes: usize,
    max_capture_string_allocation_bytes: usize,
}

struct TreeReadContext<'a> {
    focused: Option<AXUIElementRef>,
    metadata: &'a CaptureMetadata,
    policy: &'a CapturePolicy,
    policy_rejected: bool,
    string_budget: CaptureStringBudget,
}

type RunningApplication = (OwnedCf, i32, Option<String>, Option<String>);

impl Default for MacOsAccessibilityProvider {
    fn default() -> Self {
        Self {
            max_depth: 12,
            max_nodes: 4_000,
            max_field_bytes: 64 * 1024,
            max_capture_string_allocation_bytes: MAX_CAPTURE_STRING_ALLOCATION_BYTES,
        }
    }
}

impl MacOsAccessibilityProvider {
    pub fn with_limits(max_depth: usize, max_nodes: usize) -> Self {
        Self {
            max_depth,
            max_nodes,
            ..Default::default()
        }
    }

    /// Queries Accessibility trust for the calling process. Each executable
    /// that uses Accessibility must invoke this from its own process because
    /// macOS grants TCC consent per code identity.
    pub fn process_is_trusted() -> bool {
        // SAFETY: AXIsProcessTrusted takes no parameters and returns only the
        // calling process's TCC trust state.
        unsafe { AXIsProcessTrusted() }
    }

    /// Requests the system Accessibility prompt for the calling process and
    /// returns the trust state observed immediately afterward.
    pub fn request_process_trust() -> bool {
        // SAFETY: all dictionary members are process-global CoreFoundation
        // constants. CFDictionaryCreate retains them using the standard type
        // callbacks, and the owned dictionary is released after the AX call.
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt.cast::<c_void>()];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast::<c_void>(),
                ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast::<c_void>(),
            );
            if options.is_null() {
                return AXIsProcessTrusted();
            }
            let trusted = AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
            trusted
        }
    }

    fn capture_sync(&self, policy: &CapturePolicy) -> Result<ForegroundCapture, CaptureError> {
        self.capture_sync_for_target(policy, None)
    }

    fn capture_sync_for_target(
        &self,
        policy: &CapturePolicy,
        expected_target: Option<(i32, &str, Option<i64>)>,
    ) -> Result<ForegroundCapture, CaptureError> {
        if !Self::process_is_trusted() {
            return Err(CaptureError::PermissionDenied);
        }
        capture_state(
            unsafe { IsSecureEventInputEnabled() },
            session_permits_capture(),
        )?;

        // Tokio worker threads do not have AppKit's event-loop autorelease
        // pool. Keep every Objective-C +0 object bounded to this capture and
        // drain only after its metadata has been copied into owned Rust data.
        let _autorelease_pool = AutoreleasePool::new().ok_or_else(|| {
            CaptureError::Accessibility("could not create AppKit autorelease pool".into())
        })?;
        let mut string_budget = CaptureStringBudget::new(self.max_capture_string_allocation_bytes);
        let (application, pid) = foreground_application()?;
        let (bundle_id, localized_name) =
            running_application_metadata(pid, self.max_field_bytes, &mut string_budget)?;
        let (application, pid, bundle_id, mut localized_name) =
            if is_current_woof_process(pid, bundle_id.as_deref()) {
                global_non_woof_application(Some(pid), self.max_field_bytes, &mut string_budget)?
                    .ok_or(CaptureError::NoFocusedApplication)?
            } else {
                (application, pid, bundle_id, localized_name)
            };

        let window = copy_element(application.0, "AXFocusedWindow")?
            .or_else(|| copy_element(application.0, "AXMainWindow").ok().flatten())
            .ok_or_else(|| CaptureError::Accessibility("no focused window".into()))?;
        let focused = copy_element(application.0, "AXFocusedUIElement")?;
        let window_title = copy_string(
            window.0,
            "AXTitle",
            self.max_field_bytes,
            &mut string_budget,
        )?;
        let window_id = copy_positive_i64(window.0, "AXWindowNumber")?;
        let app_name = match localized_name.take() {
            Some(app_name) => app_name,
            None => copy_string(
                application.0,
                "AXTitle",
                self.max_field_bytes,
                &mut string_budget,
            )?
            .unwrap_or_else(|| format!("pid {pid}")),
        };

        // App and window rules can be decided from shallow AX metadata. Apply
        // them before even the URL-only walk, and always before reading the
        // window's recursive text tree.
        let mut metadata = CaptureMetadata {
            captured_at_ms: now_ms(),
            pid,
            app_name,
            bundle_id,
            window_title,
            window_id,
            browser_url: None,
        };
        // The inline-reply request is tied to the process and window that were
        // focused when the hotkey fired. Compare that shallow metadata before
        // either the browser URL walk or the recursive text-tree read, so a
        // focus switch cannot expose the newly foreground window's contents.
        if let Some((expected_pid, expected_window_title, expected_window_id)) = expected_target {
            validate_capture_target(
                metadata.pid,
                metadata.window_title.as_deref(),
                metadata.window_id,
                expected_pid,
                expected_window_title,
                expected_window_id,
            )?;
            if focused.is_none() {
                return Err(CaptureError::TargetMismatch);
            }
        }
        if policy.is_blacklisted_metadata(&metadata) {
            metadata.zeroize_sensitive();
            return Ok(ForegroundCapture::Blacklisted);
        }

        let mut metadata_budget = self.max_nodes;
        let initial_browser_observation = match finalize_browser_url_preflight(
            self.read_browser_url_metadata(window.0, 0, &mut metadata_budget, &mut string_budget)?,
            &metadata,
            policy,
        ) {
            BrowserUrlPreflight::Known(observation) => observation,
            BrowserUrlPreflight::Excluded => {
                metadata.zeroize_sensitive();
                return Ok(ForegroundCapture::Blacklisted);
            }
        };
        metadata.browser_url = initial_browser_observation.primary_url();
        if let Err(error) = capture_state(
            unsafe { IsSecureEventInputEnabled() },
            session_permits_capture(),
        ) {
            metadata.zeroize_sensitive();
            return Err(error);
        }

        let mut budget = self.max_nodes;
        let mut tree_context = TreeReadContext {
            focused: focused.as_ref().map(|value| value.0),
            metadata: &metadata,
            policy,
            policy_rejected: false,
            string_budget,
        };
        let mut root = if expected_target.is_some() {
            capture_contextual_reply_after_surface_preflight(
                metadata.bundle_id.as_deref(),
                &initial_browser_observation.urls,
                || self.read_node(window.0, 0, &mut budget, &mut tree_context),
            )?
        } else {
            self.read_node(window.0, 0, &mut budget, &mut tree_context)?
        };
        if tree_context.policy_rejected {
            root.zeroize_sensitive();
            metadata.zeroize_sensitive();
            return Ok(ForegroundCapture::Blacklisted);
        }
        if let Err(error) = capture_state(
            unsafe { IsSecureEventInputEnabled() },
            session_permits_capture(),
        ) {
            root.zeroize_sensitive();
            return Err(error);
        }

        // Query the global foreground target again after the recursive read.
        // Exact CF identity closes same-PID/same-title window and focused-
        // element races that metadata strings alone cannot distinguish.
        let (current_application, current_pid) = match foreground_application() {
            Ok(current) => current,
            Err(error) => {
                root.zeroize_sensitive();
                return if expected_target.is_some() {
                    Err(CaptureError::TargetMismatch)
                } else {
                    Err(error)
                };
            }
        };
        let current_window = match copy_element(current_application.0, "AXFocusedWindow") {
            Ok(Some(current_window)) => current_window,
            Ok(None) => {
                root.zeroize_sensitive();
                return if expected_target.is_some() {
                    Err(CaptureError::TargetMismatch)
                } else {
                    Ok(ForegroundCapture::Blacklisted)
                };
            }
            Err(error) => {
                root.zeroize_sensitive();
                return Err(error);
            }
        };
        let current_focused = match copy_element(current_application.0, "AXFocusedUIElement") {
            Ok(current_focused) => current_focused,
            Err(error) => {
                root.zeroize_sensitive();
                return Err(error);
            }
        };
        let current_window_id = match copy_positive_i64(current_window.0, "AXWindowNumber") {
            Ok(window_id) => window_id,
            Err(error) => {
                root.zeroize_sensitive();
                return Err(error);
            }
        };
        let same_window = unsafe {
            // SAFETY: Both AX window references remain retained for this
            // comparison and are not used after their owners are dropped.
            CFEqual(window.0, current_window.0)
        };
        let same_focused_element = match (focused.as_ref(), current_focused.as_ref()) {
            (Some(expected), Some(current)) => unsafe {
                // SAFETY: Both AX element references are retained here.
                CFEqual(expected.0, current.0)
            },
            (None, None) => true,
            _ => false,
        };
        if !capture_focus_observation_is_stable(
            metadata.pid,
            current_pid,
            same_window,
            same_focused_element,
            expected_target.and_then(|target| target.2),
            metadata.window_id,
            current_window_id,
        ) {
            root.zeroize_sensitive();
            return if expected_target.is_some() {
                Err(CaptureError::TargetMismatch)
            } else {
                Ok(ForegroundCapture::Blacklisted)
            };
        }

        // A browser may expose more than one visible web document, or navigate
        // while the full tree is being read. Re-run the metadata-only walk and
        // require the complete ordered URL observation plus the window title to
        // remain stable. This cannot make the AX read atomic, but it prevents a
        // first allowed or stale URL from authorizing different current text.
        let mut current_metadata_budget = self.max_nodes;
        let current_browser_search = match self.read_browser_url_metadata(
            current_window.0,
            0,
            &mut current_metadata_budget,
            &mut tree_context.string_budget,
        ) {
            Ok(search) => search,
            Err(error) => {
                root.zeroize_sensitive();
                return Err(error);
            }
        };
        let current_browser_observation =
            match finalize_browser_url_preflight(current_browser_search, &metadata, policy) {
                BrowserUrlPreflight::Known(observation) => observation,
                BrowserUrlPreflight::Excluded => {
                    root.zeroize_sensitive();
                    return Ok(ForegroundCapture::Blacklisted);
                }
            };
        let mut current_window_title = match copy_string(
            current_window.0,
            "AXTitle",
            self.max_field_bytes,
            &mut tree_context.string_budget,
        ) {
            Ok(title) => title,
            Err(error) => {
                root.zeroize_sensitive();
                return Err(error);
            }
        };
        let metadata_stable = browser_capture_metadata_is_stable(
            &metadata,
            &initial_browser_observation,
            current_window_title.as_deref(),
            current_window_id,
            expected_target.and_then(|target| target.2),
            &current_browser_observation,
        );
        zeroize_optional_string(&mut current_window_title);
        if !metadata_stable {
            root.zeroize_sensitive();
            return if expected_target.is_some() {
                Err(CaptureError::TargetMismatch)
            } else {
                Ok(ForegroundCapture::Blacklisted)
            };
        }
        if let Err(error) = capture_state(
            unsafe { IsSecureEventInputEnabled() },
            session_permits_capture(),
        ) {
            root.zeroize_sensitive();
            return Err(error);
        }

        let mut capture = metadata.into_raw_capture(root);
        // Defense in depth for providers that add future metadata fields after
        // the preflight. The daemon holds its shared policy lease throughout
        // this call and persistence, so the supplied policy stays authoritative.
        if policy.is_blacklisted(&capture) {
            capture.zeroize_sensitive();
            Ok(ForegroundCapture::Blacklisted)
        } else {
            Ok(ForegroundCapture::Captured(Box::new(capture)))
        }
    }

    /// Finds only browser-location metadata. It never reads AXTitle,
    /// AXDescription, or an arbitrary AXValue; AXValue is consulted solely
    /// for a strongly identified browser address control.
    fn read_browser_url_metadata(
        &self,
        element: AXUIElementRef,
        depth: usize,
        budget: &mut usize,
        string_budget: &mut CaptureStringBudget,
    ) -> Result<BrowserUrlSearch, CaptureError> {
        if unsafe { IsSecureEventInputEnabled() } {
            return Err(CaptureError::SecureInput);
        }
        if *budget == 0 {
            return Ok(BrowserUrlSearch::indeterminate());
        }
        if copy_bool(element, "AXHidden")?.unwrap_or(false) {
            return Ok(BrowserUrlSearch::default());
        }
        *budget -= 1;

        let protected_content = copy_bool(element, "AXProtectedContent")?.unwrap_or(false);
        // The probe API takes separate string and URL readers. Coordinate
        // their short-lived mutable access to one shared per-capture budget.
        let element_probe = {
            let string_budget_cell = RefCell::new(&mut *string_budget);
            probe_browser_url_element(
                protected_content,
                |attribute| {
                    let mut string_budget = string_budget_cell.borrow_mut();
                    copy_string(element, attribute, self.max_field_bytes, &mut string_budget)
                },
                |attribute| {
                    let mut string_budget = string_budget_cell.borrow_mut();
                    copy_url_string(element, attribute, self.max_field_bytes, &mut string_budget)
                },
            )?
        };
        resolve_browser_url_subtree(element_probe, || {
            if depth >= self.max_depth {
                return Ok(BrowserUrlSearch::indeterminate());
            }

            let children = copy_attribute(element, "AXVisibleChildren")?
                .or_else(|| copy_attribute(element, "AXChildren").ok().flatten());
            let mut search = BrowserUrlSearch::default();
            if let Some(children) = children {
                // SAFETY: `children` remains retained for the loop and borrowed
                // elements never escape the retained array's lifetime.
                unsafe {
                    if CFGetTypeID(children.0) == CFArrayGetTypeID() {
                        let count = CFArrayGetCount(children.0 as CFArrayRef).max(0);
                        for index in 0..count {
                            if *budget == 0 {
                                search.indeterminate = true;
                                break;
                            }
                            let child = CFArrayGetValueAtIndex(children.0 as CFArrayRef, index);
                            if !child.is_null() && CFGetTypeID(child) == AXUIElementGetTypeID() {
                                let child_search = self.read_browser_url_metadata(
                                    child as AXUIElementRef,
                                    depth + 1,
                                    budget,
                                    string_budget,
                                )?;
                                search.merge(child_search);
                            }
                        }
                    }
                }
            }
            if unsafe { IsSecureEventInputEnabled() } {
                return Err(CaptureError::SecureInput);
            }
            Ok(search)
        })
    }

    fn read_node(
        &self,
        element: AXUIElementRef,
        depth: usize,
        budget: &mut usize,
        context: &mut TreeReadContext<'_>,
    ) -> Result<AccessibilityNode, CaptureError> {
        if context.policy_rejected {
            return Ok(AccessibilityNode::default());
        }
        if unsafe { IsSecureEventInputEnabled() } {
            return Err(CaptureError::SecureInput);
        }
        if *budget == 0 {
            return Ok(AccessibilityNode::default());
        }
        // AX trees for browsers and rich editors may retain offscreen or
        // collapsed content. Never collect fields from nodes the application
        // explicitly marks hidden.
        if copy_bool(element, "AXHidden")?.unwrap_or(false) {
            return Ok(AccessibilityNode::default());
        }
        *budget -= 1;

        let mut role = match copy_string(
            element,
            "AXRole",
            self.max_field_bytes,
            &mut context.string_budget,
        )? {
            Some(role) => role,
            None => context.string_budget.retain_literal("AXUnknown")?,
        };
        let mut subrole = match copy_string(
            element,
            "AXSubrole",
            self.max_field_bytes,
            &mut context.string_budget,
        ) {
            Ok(subrole) => subrole,
            Err(error) => {
                role.zeroize();
                return Err(error);
            }
        };
        let protected_content = match copy_bool(element, "AXProtectedContent") {
            Ok(protected_content) => protected_content.unwrap_or(false),
            Err(error) => {
                role.zeroize();
                zeroize_optional_string(&mut subrole);
                return Err(error);
            }
        };
        let focused_by_identity = context.focused.is_some_and(|candidate| unsafe {
            // SAFETY: Both pointers remain retained for this traversal.
            CFEqual(element as CFTypeRef, candidate as CFTypeRef)
        });
        let is_focused = if focused_by_identity {
            true
        } else {
            match copy_bool(element, "AXFocused") {
                Ok(focused) => focused.unwrap_or(false),
                Err(error) => {
                    role.zeroize();
                    zeroize_optional_string(&mut subrole);
                    return Err(error);
                }
            }
        };
        let frame = match copy_frame(element) {
            Ok(frame) => frame,
            Err(error) => {
                role.zeroize();
                zeroize_optional_string(&mut subrole);
                return Err(error);
            }
        };
        let protected = protected_content || role_is_sensitive(&role, subrole.as_deref());
        if protected {
            return Ok(AccessibilityNode {
                role,
                subrole,
                frame,
                title: None,
                value: None,
                description: None,
                placeholder: None,
                identifier: None,
                url: None,
                focused: is_focused,
                protected: true,
                children: Vec::new(),
            });
        }
        if role_is_web_document(&role) && context.policy.requires_browser_url_preflight() {
            let mut current_url = match read_current_web_document_url(
                element,
                self.max_field_bytes,
                &mut context.string_budget,
            ) {
                Ok(url) => url,
                Err(error) => {
                    role.zeroize();
                    zeroize_optional_string(&mut subrole);
                    return Err(error);
                }
            };
            let authorized = web_document_is_authorized(
                context.metadata,
                context.policy,
                current_url.as_deref(),
            );
            zeroize_optional_string(&mut current_url);
            if !authorized {
                role.zeroize();
                zeroize_optional_string(&mut subrole);
                context.policy_rejected = true;
                return Ok(AccessibilityNode::default());
            }
        }
        if unsafe { IsSecureEventInputEnabled() } {
            role.zeroize();
            zeroize_optional_string(&mut subrole);
            return Err(CaptureError::SecureInput);
        }
        let mut node = AccessibilityNode {
            role,
            subrole,
            frame,
            title: None,
            value: None,
            description: None,
            placeholder: None,
            identifier: None,
            url: None,
            focused: is_focused,
            protected: false,
            children: Vec::new(),
        };
        node.title = match copy_string(
            element,
            "AXTitle",
            self.max_field_bytes,
            &mut context.string_budget,
        ) {
            Ok(value) => value,
            Err(error) => {
                zeroize_node(&mut node);
                return Err(error);
            }
        };
        node.value = match copy_string(
            element,
            "AXValue",
            self.max_field_bytes,
            &mut context.string_budget,
        ) {
            Ok(value) => value,
            Err(error) => {
                zeroize_node(&mut node);
                return Err(error);
            }
        };
        node.description = match copy_string(
            element,
            "AXDescription",
            self.max_field_bytes,
            &mut context.string_budget,
        ) {
            Ok(value) => value,
            Err(error) => {
                zeroize_node(&mut node);
                return Err(error);
            }
        };
        node.placeholder = match copy_string(
            element,
            "AXPlaceholderValue",
            self.max_field_bytes,
            &mut context.string_budget,
        ) {
            Ok(value) => value,
            Err(error) => {
                zeroize_node(&mut node);
                return Err(error);
            }
        };
        node.identifier = match copy_string(
            element,
            "AXIdentifier",
            self.max_field_bytes,
            &mut context.string_budget,
        ) {
            Ok(value) => value,
            Err(error) => {
                zeroize_node(&mut node);
                return Err(error);
            }
        };
        node.url = match copy_url_string(
            element,
            "AXURL",
            self.max_field_bytes,
            &mut context.string_budget,
        ) {
            Ok(value) => value,
            Err(error) => {
                zeroize_node(&mut node);
                return Err(error);
            }
        };
        if unsafe { IsSecureEventInputEnabled() } {
            zeroize_node(&mut node);
            return Err(CaptureError::SecureInput);
        }

        if depth >= self.max_depth {
            return Ok(node);
        }

        // Prefer the platform's visibility-filtered children when the
        // application exposes them. Fall back to AXChildren for native apps
        // that do not implement AXVisibleChildren.
        let children = match copy_attribute(element, "AXVisibleChildren") {
            Ok(children) => {
                children.or_else(|| copy_attribute(element, "AXChildren").ok().flatten())
            }
            Err(error) => {
                zeroize_node(&mut node);
                return Err(error);
            }
        };
        if let Some(children) = children {
            // SAFETY: `children` remains retained for the loop and every
            // borrowed array element is used only within that lifetime.
            unsafe {
                if CFGetTypeID(children.0) == CFArrayGetTypeID() {
                    let count = CFArrayGetCount(children.0 as CFArrayRef).max(0);
                    for index in 0..count {
                        if *budget == 0 {
                            break;
                        }
                        let child = CFArrayGetValueAtIndex(children.0 as CFArrayRef, index);
                        if !child.is_null() && CFGetTypeID(child) == AXUIElementGetTypeID() {
                            let child =
                                self.read_node(child as AXUIElementRef, depth + 1, budget, context);
                            match child {
                                Ok(child) if !context.policy_rejected => node.children.push(child),
                                Ok(mut child) => {
                                    child.zeroize_sensitive();
                                    zeroize_node(&mut node);
                                    return Ok(AccessibilityNode::default());
                                }
                                Err(error) => {
                                    zeroize_node(&mut node);
                                    return Err(error);
                                }
                            }
                        }
                    }
                }
            }
        }
        if unsafe { IsSecureEventInputEnabled() } {
            zeroize_node(&mut node);
            return Err(CaptureError::SecureInput);
        }
        Ok(node)
    }
}

fn capture_state(secure_input: bool, session_available: bool) -> Result<(), CaptureError> {
    if secure_input {
        Err(CaptureError::SecureInput)
    } else if !session_available {
        Err(CaptureError::NoFocusedApplication)
    } else {
        Ok(())
    }
}

fn zeroize_node(node: &mut AccessibilityNode) {
    node.zeroize_sensitive();
}

#[async_trait]
impl AccessibilityProvider for MacOsAccessibilityProvider {
    async fn capture_foreground(
        &self,
        policy: &CapturePolicy,
    ) -> Result<ForegroundCapture, CaptureError> {
        self.capture_sync(policy)
    }

    async fn capture_foreground_for_target(
        &self,
        policy: &CapturePolicy,
        expected_pid: i32,
        expected_window_title: &str,
        expected_window_id: Option<i64>,
    ) -> Result<ForegroundCapture, CaptureError> {
        self.capture_sync_for_target(
            policy,
            Some((expected_pid, expected_window_title, expected_window_id)),
        )
    }
}

#[derive(Debug)]
struct CaptureStringBudget {
    remaining_bytes: usize,
}

impl CaptureStringBudget {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            remaining_bytes: maximum_bytes,
        }
    }

    /// Reserves the exact capacity that a successful conversion retains.
    ///
    /// A budget-limited conversion may still succeed when the complete UTF-8
    /// value fits in the remaining capacity. If it does not, the reservation
    /// is wiped and refunded before a constant resource-limit error is
    /// returned. Successful reservations are intentionally never refunded:
    /// this bounds cumulative allocation even for probe strings dropped before
    /// the complete capture is assembled.
    fn with_conversion_capacity<T>(
        &mut self,
        requested_capacity: usize,
        convert: impl FnOnce(usize) -> Option<T>,
    ) -> Result<Option<T>, CaptureError> {
        debug_assert!(requested_capacity > 0);
        let reserved_capacity = requested_capacity.min(self.remaining_bytes);
        if reserved_capacity == 0 {
            return Err(capture_string_budget_exhausted());
        }
        self.remaining_bytes -= reserved_capacity;
        if let Some(value) = convert(reserved_capacity) {
            return Ok(Some(value));
        }

        self.remaining_bytes += reserved_capacity;
        if reserved_capacity < requested_capacity {
            Err(capture_string_budget_exhausted())
        } else {
            // Preserve the existing whole-field behavior for values that do
            // not fit the independent per-field conversion limit.
            Ok(None)
        }
    }

    fn retain_literal(&mut self, value: &str) -> Result<String, CaptureError> {
        if value.len() > self.remaining_bytes {
            return Err(capture_string_budget_exhausted());
        }
        self.remaining_bytes -= value.len();
        Ok(value.to_owned())
    }

    #[cfg(test)]
    fn remaining_bytes(&self) -> usize {
        self.remaining_bytes
    }
}

fn capture_string_budget_exhausted() -> CaptureError {
    CaptureError::Accessibility(CAPTURE_STRING_BUDGET_EXHAUSTED.to_owned())
}

fn copy_element(element: AXUIElementRef, attribute: &str) -> Result<Option<OwnedCf>, CaptureError> {
    let value = copy_attribute(element, attribute)?;
    Ok(value.filter(|candidate| unsafe {
        // SAFETY: The candidate is retained by `OwnedCf`.
        CFGetTypeID(candidate.0) == AXUIElementGetTypeID()
    }))
}

fn copy_string(
    element: AXUIElementRef,
    attribute: &str,
    maximum_bytes: usize,
    string_budget: &mut CaptureStringBudget,
) -> Result<Option<String>, CaptureError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    unsafe {
        if CFGetTypeID(value.0) != CFStringGetTypeID() {
            return Ok(None);
        }
        // SAFETY: The type check precedes the CFString conversion and the
        // object is retained for the conversion.
        cf_string_to_rust(value.0 as CFStringRef, maximum_bytes, string_budget)
    }
}

fn copy_url_string(
    element: AXUIElementRef,
    attribute: &str,
    maximum_bytes: usize,
    string_budget: &mut CaptureStringBudget,
) -> Result<Option<String>, CaptureError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    // SAFETY: The type is checked before treating the value as CFString/CFURL,
    // and the copied attribute remains retained for this entire conversion.
    unsafe {
        let type_id = CFGetTypeID(value.0);
        if type_id == CFStringGetTypeID() {
            cf_string_to_rust(value.0 as CFStringRef, maximum_bytes, string_budget)
        } else if type_id == CFURLGetTypeID() {
            cf_string_to_rust(CFURLGetString(value.0), maximum_bytes, string_budget)
        } else {
            Ok(None)
        }
    }
}

unsafe fn cf_string_to_rust(
    value: CFStringRef,
    maximum_bytes: usize,
    string_budget: &mut CaptureStringBudget,
) -> Result<Option<String>, CaptureError> {
    if value.is_null() {
        return Ok(None);
    }
    let length = CFStringGetLength(value);
    let maximum = CFStringGetMaximumSizeForEncoding(length, UTF8)
        .saturating_add(1)
        .max(1) as usize;
    let requested_capacity = maximum.min(maximum_bytes.saturating_add(1)).max(1);
    string_budget.with_conversion_capacity(requested_capacity, |capacity| {
        let mut buffer = vec![0_u8; capacity];
        if !CFStringGetCString(
            value,
            buffer.as_mut_ptr().cast(),
            buffer.len() as CFIndex,
            UTF8,
        ) {
            buffer.zeroize();
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
    buffer[length..].zeroize();
    buffer.truncate(length);
    match String::from_utf8(buffer) {
        Ok(value) => Some(value),
        Err(error) => {
            let mut buffer = error.into_bytes();
            buffer.zeroize();
            None
        }
    }
}

fn copy_bool(element: AXUIElementRef, attribute: &str) -> Result<Option<bool>, CaptureError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    // SAFETY: The type check precedes `CFBooleanGetValue`.
    unsafe {
        Ok((CFGetTypeID(value.0) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(value.0)))
    }
}

fn copy_positive_i64(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<Option<i64>, CaptureError> {
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

fn copy_frame(element: AXUIElementRef) -> Result<Option<AccessibilityRect>, CaptureError> {
    if let Some(frame) = copy_ax_value::<CGRect>(element, "AXFrame", AX_VALUE_CGRECT)? {
        if let Some(frame) = integer_rect(frame.origin, frame.size) {
            return Ok(Some(frame));
        }
    }

    let position = copy_ax_value::<CGPoint>(element, "AXPosition", AX_VALUE_CGPOINT)?;
    let size = copy_ax_value::<CGSize>(element, "AXSize", AX_VALUE_CGSIZE)?;
    Ok(position
        .zip(size)
        .and_then(|(position, size)| integer_rect(position, size)))
}

fn copy_ax_value<T: Default>(
    element: AXUIElementRef,
    attribute: &str,
    value_type: i32,
) -> Result<Option<T>, CaptureError> {
    let Some(value) = copy_attribute(element, attribute)? else {
        return Ok(None);
    };
    // SAFETY: The AXValue type is checked before its fixed-size payload is
    // copied into a correctly sized output value.
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

fn integer_rect(position: CGPoint, size: CGSize) -> Option<AccessibilityRect> {
    let rounded = AccessibilityRect {
        x: finite_rounded_i64(position.x)?,
        y: finite_rounded_i64(position.y)?,
        width: finite_rounded_i64(size.width)?,
        height: finite_rounded_i64(size.height)?,
    };
    if rounded.width <= 0
        || rounded.height <= 0
        || rounded.x.checked_add(rounded.width).is_none()
        || rounded.y.checked_add(rounded.height).is_none()
    {
        None
    } else {
        Some(rounded)
    }
}

fn finite_rounded_i64(value: f64) -> Option<i64> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.round();
    if rounded <= i64::MIN as f64 || rounded >= i64::MAX as f64 {
        None
    } else {
        Some(rounded as i64)
    }
}

fn copy_attribute(
    element: AXUIElementRef,
    attribute: &str,
) -> Result<Option<OwnedCf>, CaptureError> {
    let c_attribute = CString::new(attribute)
        .map_err(|_| CaptureError::Accessibility("invalid attribute name".into()))?;
    // SAFETY: The input is a valid nul-terminated UTF-8 string. The returned
    // CFString and copied attribute are owned and wrapped immediately.
    unsafe {
        let attribute = OwnedCf::new(CFStringCreateWithCString(
            ptr::null(),
            c_attribute.as_ptr(),
            UTF8,
        ))
        .ok_or_else(|| CaptureError::Accessibility("could not create attribute".into()))?;
        let mut value: CFTypeRef = ptr::null();
        let error = AXUIElementCopyAttributeValue(element, attribute.0, &mut value);
        if error == AX_ERROR_SUCCESS {
            Ok(OwnedCf::new(value))
        } else {
            // Missing/unsupported attributes are normal across applications.
            Ok(None)
        }
    }
}

fn session_permits_capture() -> bool {
    // SAFETY: CoreGraphics returns a create-rule dictionary describing only
    // the current WindowServer session. It is retained for all key lookups.
    let Some(session) = OwnedCf::new(unsafe { CGSessionCopyCurrentDictionary() as CFTypeRef })
    else {
        return false;
    };
    dictionary_bool(session.0 as CFDictionaryRef, "kCGSSessionOnConsoleKey").unwrap_or(false)
        && dictionary_bool(session.0 as CFDictionaryRef, "kCGSessionLoginDoneKey").unwrap_or(false)
        && !dictionary_bool(session.0 as CFDictionaryRef, "CGSSessionScreenIsLocked")
            .unwrap_or(false)
}

fn dictionary_bool(dictionary: CFDictionaryRef, key: &str) -> Option<bool> {
    let key = CString::new(key).ok()?;
    // SAFETY: The temporary CFString remains retained through lookup; values
    // borrowed from the retained dictionary are used only for this type check.
    unsafe {
        let key = OwnedCf::new(CFStringCreateWithCString(ptr::null(), key.as_ptr(), UTF8))?;
        let value = CFDictionaryGetValue(dictionary, key.0);
        if value.is_null() || CFGetTypeID(value) != CFBooleanGetTypeID() {
            None
        } else {
            Some(CFBooleanGetValue(value))
        }
    }
}

fn validated_web_url(mut value: Option<String>) -> Option<String> {
    if value.as_deref().is_some_and(is_web_url) {
        value
    } else {
        if let Some(value) = &mut value {
            value.zeroize();
        }
        None
    }
}

fn read_current_web_document_url(
    element: AXUIElementRef,
    maximum_bytes: usize,
    string_budget: &mut CaptureStringBudget,
) -> Result<Option<String>, CaptureError> {
    let document = validated_web_url(copy_url_string(
        element,
        "AXDocument",
        maximum_bytes,
        string_budget,
    )?);
    if document.is_some() {
        return Ok(document);
    }
    Ok(validated_web_url(copy_url_string(
        element,
        "AXURL",
        maximum_bytes,
        string_budget,
    )?))
}

fn web_document_is_authorized(
    metadata: &CaptureMetadata,
    policy: &CapturePolicy,
    current_url: Option<&str>,
) -> bool {
    current_url
        .is_some_and(|url| !policy.is_blacklisted_metadata_with_browser_url(metadata, Some(url)))
}

#[derive(Debug, PartialEq, Eq)]
enum BrowserUrlElementProbe {
    Found(String),
    Protected,
    WebDocumentWithoutUrl,
    Continue,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct BrowserUrlSearch {
    urls: Vec<String>,
    unknown_web_document: bool,
    indeterminate: bool,
}

impl BrowserUrlSearch {
    fn found(url: String) -> Self {
        Self {
            urls: vec![url],
            ..Self::default()
        }
    }

    fn unknown_web_document() -> Self {
        Self {
            urls: Vec::new(),
            unknown_web_document: true,
            indeterminate: false,
        }
    }

    fn indeterminate() -> Self {
        Self {
            urls: Vec::new(),
            unknown_web_document: false,
            indeterminate: true,
        }
    }

    fn merge(&mut self, mut other: Self) {
        self.urls.append(&mut other.urls);
        self.unknown_web_document |= other.unknown_web_document;
        self.indeterminate |= other.indeterminate;
    }
}

impl Drop for BrowserUrlSearch {
    fn drop(&mut self) {
        zeroize_strings(&mut self.urls);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct BrowserUrlObservation {
    urls: Vec<String>,
}

impl BrowserUrlObservation {
    fn primary_url(&self) -> Option<String> {
        self.urls.first().cloned()
    }
}

impl Drop for BrowserUrlObservation {
    fn drop(&mut self) {
        zeroize_strings(&mut self.urls);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum BrowserUrlPreflight {
    Known(BrowserUrlObservation),
    Excluded,
}

fn finalize_browser_url_preflight(
    mut search: BrowserUrlSearch,
    metadata: &CaptureMetadata,
    policy: &CapturePolicy,
) -> BrowserUrlPreflight {
    if (search.unknown_web_document || search.indeterminate)
        && policy.requires_browser_url_preflight()
    {
        return BrowserUrlPreflight::Excluded;
    }

    let blacklisted = if search.urls.is_empty() {
        policy.is_blacklisted_metadata_with_browser_url(metadata, None)
    } else {
        search.urls.iter().any(|url| {
            policy.is_blacklisted_metadata_with_browser_url(metadata, Some(url.as_str()))
        })
    };
    if blacklisted {
        BrowserUrlPreflight::Excluded
    } else {
        BrowserUrlPreflight::Known(BrowserUrlObservation {
            urls: std::mem::take(&mut search.urls),
        })
    }
}

fn browser_capture_metadata_is_stable(
    metadata: &CaptureMetadata,
    initial_urls: &BrowserUrlObservation,
    current_window_title: Option<&str>,
    current_window_id: Option<i64>,
    expected_window_id: Option<i64>,
    current_urls: &BrowserUrlObservation,
) -> bool {
    metadata.window_title.as_deref() == current_window_title
        && expected_window_id.is_none_or(|window_id| {
            metadata.window_id == Some(window_id) && current_window_id == Some(window_id)
        })
        && initial_urls == current_urls
}

fn capture_focus_observation_is_stable(
    expected_pid: i32,
    current_pid: i32,
    same_window: bool,
    same_focused_element: bool,
    expected_window_id: Option<i64>,
    initial_window_id: Option<i64>,
    current_window_id: Option<i64>,
) -> bool {
    expected_pid == current_pid
        && same_window
        && same_focused_element
        && expected_window_id.is_none_or(|window_id| {
            initial_window_id == Some(window_id) && current_window_id == Some(window_id)
        })
}

fn zeroize_optional_string(value: &mut Option<String>) {
    if let Some(value) = value {
        value.zeroize();
    }
    *value = None;
}

fn zeroize_strings(values: &mut Vec<String>) {
    for value in values.iter_mut() {
        value.zeroize();
    }
    values.clear();
}

fn resolve_browser_url_subtree(
    element: BrowserUrlElementProbe,
    read_descendants: impl FnOnce() -> Result<BrowserUrlSearch, CaptureError>,
) -> Result<BrowserUrlSearch, CaptureError> {
    match element {
        BrowserUrlElementProbe::Found(url) => Ok(BrowserUrlSearch::found(url)),
        BrowserUrlElementProbe::Protected => Ok(BrowserUrlSearch::default()),
        BrowserUrlElementProbe::WebDocumentWithoutUrl => {
            Ok(BrowserUrlSearch::unknown_web_document())
        }
        BrowserUrlElementProbe::Continue => read_descendants(),
    }
}

fn probe_browser_url_element(
    protected_content: bool,
    mut read_string: impl FnMut(&str) -> Result<Option<String>, CaptureError>,
    mut read_url: impl FnMut(&str) -> Result<Option<String>, CaptureError>,
) -> Result<BrowserUrlElementProbe, CaptureError> {
    let role = Zeroizing::new(read_string("AXRole")?.unwrap_or_else(|| "AXUnknown".to_owned()));
    let subrole = read_string("AXSubrole")?.map(Zeroizing::new);
    if protected_content || role_is_sensitive(&role, subrole.as_ref().map(|value| value.as_str())) {
        return Ok(BrowserUrlElementProbe::Protected);
    }

    let identifier = read_string("AXIdentifier")?.map(Zeroizing::new);
    let web_document = role_is_web_document(&role);
    let address_control = role_is_address_control(
        &role,
        subrole.as_ref().map(|value| value.as_str()),
        identifier.as_ref().map(|value| value.as_str()),
    );

    // Links also expose AXURL, so URL-bearing attributes are read only from a
    // web document or a browser location control identified without content.
    if web_document {
        if let Some(url) = validated_web_url(read_url("AXDocument")?) {
            return Ok(BrowserUrlElementProbe::Found(url));
        }
        if let Some(url) = validated_web_url(read_url("AXURL")?) {
            return Ok(BrowserUrlElementProbe::Found(url));
        }
        return Ok(BrowserUrlElementProbe::WebDocumentWithoutUrl);
    }
    if address_control {
        if let Some(url) = validated_web_url(read_url("AXURL")?) {
            return Ok(BrowserUrlElementProbe::Found(url));
        }
        if let Some(url) = validated_web_url(read_string("AXValue")?) {
            return Ok(BrowserUrlElementProbe::Found(url));
        }
    }

    Ok(BrowserUrlElementProbe::Continue)
}

fn role_is_address_control(role: &str, subrole: Option<&str>, identifier: Option<&str>) -> bool {
    let role = Zeroizing::new(role.to_ascii_lowercase());
    let subrole = Zeroizing::new(subrole.unwrap_or_default().to_ascii_lowercase());
    let identifier = Zeroizing::new(identifier.unwrap_or_default().to_ascii_lowercase());
    role.contains("addressfield")
        || role.contains("locationfield")
        || subrole.contains("addressfield")
        || subrole.contains("locationfield")
        || identifier.contains("address")
        || identifier.contains("omnibox")
        || identifier.contains("locationbar")
        || identifier.contains("location-bar")
        || identifier.contains("urlbar")
        || identifier.contains("url-bar")
        || matches!(identifier.as_str(), "url" | "url_field" | "url-field")
}

fn role_is_web_document(role: &str) -> bool {
    role.eq_ignore_ascii_case("AXWebArea")
}

fn role_is_sensitive(role: &str, subrole: Option<&str>) -> bool {
    let combined = format!("{role} {}", subrole.unwrap_or_default()).to_ascii_lowercase();
    combined.contains("securetextfield")
        || combined.contains("password")
        || combined.contains("secure text")
}

fn is_web_url(value: &str) -> bool {
    let lower = Zeroizing::new(value.trim().to_ascii_lowercase());
    lower.starts_with("https://") || lower.starts_with("http://")
}

fn running_application_metadata(
    pid: i32,
    maximum_bytes: usize,
    string_budget: &mut CaptureStringBudget,
) -> Result<(Option<String>, Option<String>), CaptureError> {
    unsafe {
        // SAFETY: Objective-C selectors are static nul-terminated strings.
        // The returned NSRunningApplication/NSString objects are autoreleased
        // and remain valid for this synchronous call.
        let class_name = CString::new("NSRunningApplication").expect("static class");
        let class = objc_getClass(class_name.as_ptr());
        if class.is_null() {
            return Ok((None, None));
        }
        let run_sel = selector("runningApplicationWithProcessIdentifier:");
        let application = objc_msg_send_id_i32(class, run_sel, pid);
        if application.is_null() {
            return Ok((None, None));
        }
        let bundle = objc_msg_send_id(application, selector("bundleIdentifier"));
        let bundle_id = ns_string(bundle, maximum_bytes, string_budget)?;
        let name = objc_msg_send_id(application, selector("localizedName"));
        let localized_name = ns_string(name, maximum_bytes, string_budget)?;
        Ok((bundle_id, localized_name))
    }
}

/// Resolve the actual frontmost process independently of the system-wide AX
/// focused-application attribute. The latter is occasionally absent for a
/// trusted background/agent process even while a normal application is
/// frontmost.
fn foreground_application() -> Result<(OwnedCf, i32), CaptureError> {
    // Preserve the normal public AX route first. It is authoritative when the
    // system-wide focused-application attribute is available.
    if let Some(system) = OwnedCf::new(unsafe { AXUIElementCreateSystemWide() as CFTypeRef }) {
        if let Some(application) = copy_element(system.0, "AXFocusedApplication")? {
            if let Some(pid) = application_pid(application.0) {
                return Ok((application, pid));
            }
        }
    }

    // Carbon's Process Manager queries the global foreground process for the
    // current login session and does not require this background daemon to
    // have an AppKit activation context.
    if let Some(pid) = carbon_frontmost_application_pid() {
        // SAFETY: `pid` is a live positive process identifier returned by the
        // Process Manager. The create-rule AX object is immediately owned.
        if let Some(application) =
            OwnedCf::new(unsafe { AXUIElementCreateApplication(pid) as CFTypeRef })
        {
            return Ok((application, pid));
        }
    }

    // Preserve NSWorkspace as a final fallback for workspace transitions in
    // which the Process Manager does not return a front process.
    if let Some(pid) = frontmost_application_pid() {
        // SAFETY: `pid` is a live positive process identifier reported by
        // NSWorkspace. The create-rule AX object is immediately owned.
        if let Some(application) =
            OwnedCf::new(unsafe { AXUIElementCreateApplication(pid) as CFTypeRef })
        {
            return Ok((application, pid));
        }
    }

    Err(CaptureError::NoFocusedApplication)
}

fn carbon_frontmost_application_pid() -> Option<i32> {
    let mut process = ProcessSerialNumber::default();
    // SAFETY: `process` points to an initialized C-layout structure which
    // GetFrontProcess fills synchronously and does not retain.
    let front_status = unsafe { GetFrontProcess(&mut process) };
    if front_status != 0 {
        return None;
    }

    let mut pid = 0_i32;
    // SAFETY: `process` was initialized successfully by GetFrontProcess and
    // `pid` is writable storage. Neither pointer is retained by Carbon.
    let pid_status = unsafe { GetProcessPID(&process, &mut pid) };
    carbon_process_identifier(front_status, pid_status, pid)
}

fn carbon_process_identifier(front_status: OSErr, pid_status: OSStatus, pid: i32) -> Option<i32> {
    (front_status == 0 && pid_status == NO_ERR)
        .then(|| normalize_process_identifier(pid))
        .flatten()
}

fn global_non_woof_application(
    excluded_pid: Option<i32>,
    maximum_bytes: usize,
    string_budget: &mut CaptureStringBudget,
) -> Result<Option<RunningApplication>, CaptureError> {
    let candidates = [
        carbon_frontmost_application_pid(),
        frontmost_application_pid(),
    ];
    let mut previous_pid = None;

    for pid in candidates.into_iter().flatten() {
        if Some(pid) == excluded_pid || Some(pid) == previous_pid {
            continue;
        }
        previous_pid = Some(pid);

        // SAFETY: Both providers return a positive process identifier for the
        // current login session. The create-rule AX object is owned here.
        let Some(application) =
            OwnedCf::new(unsafe { AXUIElementCreateApplication(pid) as CFTypeRef })
        else {
            continue;
        };
        let (bundle_id, localized_name) =
            running_application_metadata(pid, maximum_bytes, string_budget)?;
        if is_current_woof_process(pid, bundle_id.as_deref()) {
            continue;
        }
        return Ok(Some((application, pid, bundle_id, localized_name)));
    }
    Ok(None)
}

fn is_current_woof_process(pid: i32, bundle_id: Option<&str>) -> bool {
    let own_pid = i32::try_from(std::process::id()).unwrap_or(-1);
    // SAFETY: getppid has no arguments and returns process-global state.
    let parent_pid = unsafe { getppid() };
    is_woof_process_candidate(pid, bundle_id, own_pid, parent_pid)
}

fn is_woof_process_candidate(
    pid: i32,
    bundle_id: Option<&str>,
    own_pid: i32,
    parent_pid: i32,
) -> bool {
    bundle_id == Some(WOOF_BUNDLE_ID)
        || (bundle_id.is_none() && (pid == own_pid || pid == parent_pid))
}

fn application_pid(application: AXUIElementRef) -> Option<i32> {
    let mut pid = 0_i32;
    // SAFETY: `application` is a retained AX application element and `pid`
    // points to initialized writable memory.
    let error = unsafe { AXUIElementGetPid(application, &mut pid) };
    (error == AX_ERROR_SUCCESS && pid > 0).then_some(pid)
}

fn frontmost_application_pid() -> Option<i32> {
    unsafe {
        // SAFETY: Objective-C selectors are static nul-terminated strings.
        // NSWorkspace is process-global and the returned frontmost
        // NSRunningApplication remains valid for this synchronous call.
        let class_name = CString::new("NSWorkspace").expect("static class");
        let class = objc_getClass(class_name.as_ptr());
        if class.is_null() {
            return None;
        }
        let workspace = objc_msg_send_id(class, selector("sharedWorkspace"));
        if workspace.is_null() {
            return None;
        }
        let application = objc_msg_send_id(workspace, selector("frontmostApplication"));
        if application.is_null() {
            return None;
        }

        let raw_pid = objc_msg_send_pid(application, selector("processIdentifier"));
        normalize_process_identifier(raw_pid)
    }
}

fn normalize_process_identifier(raw_pid: i32) -> Option<i32> {
    (raw_pid > 0).then_some(raw_pid)
}

unsafe fn objc_msg_send_pid(receiver: *mut c_void, selector: *mut c_void) -> i32 {
    // Objective-C messages must be called through a function pointer matching
    // the method's actual return type. `processIdentifier` returns pid_t
    // (signed 32-bit), unlike the object-returning messages above.
    let send = std::mem::transmute::<
        *const (),
        unsafe extern "C" fn(*mut c_void, *mut c_void) -> i32,
    >(objc_msgSend as *const ());
    send(receiver, selector)
}

unsafe fn objc_msg_send_id(receiver: *mut c_void, selector: *mut c_void) -> *mut c_void {
    let send = std::mem::transmute::<
        *const (),
        unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void,
    >(objc_msgSend as *const ());
    send(receiver, selector)
}

unsafe fn objc_msg_send_id_i32(
    receiver: *mut c_void,
    selector: *mut c_void,
    value: i32,
) -> *mut c_void {
    let send = std::mem::transmute::<
        *const (),
        unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> *mut c_void,
    >(objc_msgSend as *const ());
    send(receiver, selector, value)
}

unsafe fn objc_msg_send_void(receiver: *mut c_void, selector: *mut c_void) {
    // Match Objective-C methods such as -[NSAutoreleasePool drain], which do
    // not have the object-pointer return ABI used by the generic declaration.
    let send = std::mem::transmute::<*const (), unsafe extern "C" fn(*mut c_void, *mut c_void)>(
        objc_msgSend as *const (),
    );
    send(receiver, selector);
}

unsafe fn selector(name: &str) -> *mut c_void {
    let name = CString::new(name).expect("static selector");
    sel_registerName(name.as_ptr())
}

unsafe fn ns_string(
    value: *mut c_void,
    maximum_bytes: usize,
    string_budget: &mut CaptureStringBudget,
) -> Result<Option<String>, CaptureError> {
    if value.is_null() {
        return Ok(None);
    }
    // NSString and CFString are toll-free bridged. Use the same bounded,
    // budget-charged conversion as AX values so AppKit metadata cannot allocate
    // outside the per-capture aggregate limit.
    if CFGetTypeID(value) != CFStringGetTypeID() {
        return Ok(None);
    }
    cf_string_to_rust(value as CFStringRef, maximum_bytes, string_budget)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

struct OwnedCf(CFTypeRef);

impl OwnedCf {
    fn new(value: CFTypeRef) -> Option<Self> {
        (!value.is_null()).then_some(Self(value))
    }
}

impl Drop for OwnedCf {
    fn drop(&mut self) {
        // SAFETY: `OwnedCf` is constructed only for create/copy-rule objects
        // and releases each object exactly once.
        unsafe { CFRelease(self.0) };
    }
}

unsafe impl Send for OwnedCf {}
unsafe impl Sync for OwnedCf {}

struct AutoreleasePool(*mut c_void);

impl AutoreleasePool {
    fn new() -> Option<Self> {
        unsafe {
            // SAFETY: NSAutoreleasePool is provided by Foundation (loaded by
            // AppKit). `new` returns a +1 object owned by this guard.
            let class_name = CString::new("NSAutoreleasePool").expect("static class");
            let class = objc_getClass(class_name.as_ptr());
            if class.is_null() {
                return None;
            }
            let pool = objc_msg_send_id(class, selector("new"));
            (!pool.is_null()).then_some(Self(pool))
        }
    }
}

impl Drop for AutoreleasePool {
    fn drop(&mut self) {
        // SAFETY: The guard owns this NSAutoreleasePool and drains it once on
        // the same synchronous capture thread where it was created.
        unsafe { objc_msg_send_void(self.0, selector("drain")) };
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::ffi::{c_void, CString};
    use std::ptr;

    use crate::{BlacklistKind, BlacklistRule, CaptureMetadata, CapturePolicy};

    use super::{
        browser_capture_metadata_is_stable, capture_focus_observation_is_stable, capture_state,
        carbon_process_identifier, finalize_browser_url_preflight, integer_rect,
        is_woof_process_candidate, normalize_process_identifier, ns_string,
        nul_terminated_utf8_buffer_into_string, probe_browser_url_element,
        resolve_browser_url_subtree, role_is_web_document, web_document_is_authorized,
        zeroize_node, AccessibilityNode, AccessibilityRect, BrowserUrlElementProbe,
        BrowserUrlObservation, BrowserUrlPreflight, BrowserUrlSearch, CGPoint, CGSize,
        CaptureError, CaptureStringBudget, OwnedCf, CAPTURE_STRING_BUDGET_EXHAUSTED,
        MAX_CAPTURE_STRING_ALLOCATION_BYTES, UTF8, WOOF_BUNDLE_ID,
    };

    fn metadata() -> CaptureMetadata {
        CaptureMetadata {
            captured_at_ms: 1,
            pid: 42,
            app_name: "Safari".to_owned(),
            bundle_id: Some("com.apple.Safari".to_owned()),
            window_title: Some("Current page".to_owned()),
            window_id: Some(9_001),
            browser_url: None,
        }
    }

    #[test]
    fn process_identifier_requires_positive_pid_t_range() {
        assert_eq!(normalize_process_identifier(1), Some(1));
        assert_eq!(normalize_process_identifier(i32::MAX), Some(i32::MAX));
        assert_eq!(normalize_process_identifier(0), None);
        assert_eq!(normalize_process_identifier(-1), None);
        assert_eq!(normalize_process_identifier(i32::MIN), None);
    }

    #[test]
    fn carbon_process_lookup_requires_both_success_statuses_and_valid_pid() {
        assert_eq!(carbon_process_identifier(0, 0, 42), Some(42));
        assert_eq!(carbon_process_identifier(-1, 0, 42), None);
        assert_eq!(carbon_process_identifier(0, -1, 42), None);
        assert_eq!(carbon_process_identifier(0, 0, 0), None);
        assert_eq!(carbon_process_identifier(0, 0, -42), None);
    }

    #[test]
    fn self_detection_preserves_non_woof_parent_apps() {
        assert!(is_woof_process_candidate(
            42,
            Some(WOOF_BUNDLE_ID),
            100,
            200
        ));
        assert!(is_woof_process_candidate(100, None, 100, 200));
        assert!(is_woof_process_candidate(200, None, 100, 200));
        assert!(!is_woof_process_candidate(
            200,
            Some("com.apple.TextEdit"),
            100,
            200
        ));
        assert!(!is_woof_process_candidate(300, None, 100, 200));
    }

    #[test]
    fn capture_state_fails_closed_when_security_state_changes() {
        assert!(capture_state(false, true).is_ok());
        assert!(matches!(
            capture_state(true, true),
            Err(CaptureError::SecureInput)
        ));
        assert!(matches!(
            capture_state(false, false),
            Err(CaptureError::NoFocusedApplication)
        ));
    }

    #[test]
    fn accessibility_geometry_is_rounded_to_valid_integer_rectangles() {
        assert_eq!(
            integer_rect(
                CGPoint { x: -10.4, y: 20.6 },
                CGSize {
                    width: 399.6,
                    height: 40.4,
                },
            ),
            Some(AccessibilityRect {
                x: -10,
                y: 21,
                width: 400,
                height: 40,
            })
        );
    }

    #[test]
    fn accessibility_geometry_rejects_nonfinite_empty_and_overflowing_rectangles() {
        for (position, size) in [
            (
                CGPoint {
                    x: f64::NAN,
                    y: 0.0,
                },
                CGSize {
                    width: 10.0,
                    height: 10.0,
                },
            ),
            (
                CGPoint { x: 0.0, y: 0.0 },
                CGSize {
                    width: 0.4,
                    height: 10.0,
                },
            ),
            (
                CGPoint { x: 0.0, y: 0.0 },
                CGSize {
                    width: -1.0,
                    height: 10.0,
                },
            ),
            (
                CGPoint {
                    x: i64::MAX as f64,
                    y: 0.0,
                },
                CGSize {
                    width: 10.0,
                    height: 10.0,
                },
            ),
        ] {
            assert_eq!(integer_rect(position, size), None);
        }
    }

    #[test]
    fn aggregate_string_budget_caps_all_maximum_sized_node_fields() {
        let mut budget = CaptureStringBudget::new(MAX_CAPTURE_STRING_ALLOCATION_BYTES);
        let requested_capacity = (64 * 1024) + 1;
        let mut accepted = 0;

        for _ in 0..(4_000 * 7) {
            let converted = budget.with_conversion_capacity(requested_capacity, |capacity| {
                (capacity == requested_capacity).then_some(())
            });
            match converted {
                Ok(Some(())) => accepted += 1,
                Err(CaptureError::Accessibility(message))
                    if message == CAPTURE_STRING_BUDGET_EXHAUSTED =>
                {
                    break;
                }
                other => panic!("unexpected budget result: {other:?}"),
            }
        }

        assert_eq!(
            accepted,
            MAX_CAPTURE_STRING_ALLOCATION_BYTES / requested_capacity
        );
        assert!(accepted < 4_000 * 7);
        assert!(budget.remaining_bytes() < requested_capacity);
    }

    #[test]
    fn aggregate_string_budget_preserves_complete_multibyte_utf8_without_a_copy() {
        let source = "Grüße";
        let mut conversion_buffer = Vec::with_capacity(32);
        conversion_buffer.extend_from_slice(source.as_bytes());
        conversion_buffer.push(0);
        conversion_buffer.resize(32, 0);
        let mut budget = CaptureStringBudget::new(conversion_buffer.capacity());

        let converted = budget
            .with_conversion_capacity(conversion_buffer.capacity(), |capacity| {
                assert_eq!(capacity, conversion_buffer.capacity());
                nul_terminated_utf8_buffer_into_string(conversion_buffer)
            })
            .expect("complete conversion")
            .expect("UTF-8 string");

        assert_eq!(converted, source);
        assert_eq!(converted.capacity(), 32);
        assert_eq!(budget.remaining_bytes(), 0);
    }

    #[test]
    fn aggregate_string_budget_rejects_the_next_field_whole_and_refunds_failure() {
        let secret = "do-not-fragment";
        let mut budget = CaptureStringBudget::new(4);
        let observed_capacity = Cell::new(0);

        let error = budget
            .with_conversion_capacity(secret.len() + 1, |capacity| {
                observed_capacity.set(capacity);
                (secret.len() < capacity).then(|| secret.to_owned())
            })
            .expect_err("the complete field exceeds the remaining budget");

        assert_eq!(observed_capacity.get(), 4);
        assert_eq!(budget.remaining_bytes(), 4);
        assert!(matches!(
            error,
            CaptureError::Accessibility(ref message)
                if message == CAPTURE_STRING_BUDGET_EXHAUSTED
                    && !message.contains(secret)
        ));
    }

    #[test]
    fn independent_field_overflow_remains_a_whole_field_skip() {
        let mut budget = CaptureStringBudget::new(64);
        let converted: Option<String> = budget
            .with_conversion_capacity(32, |_| None)
            .expect("an unconstrained failed conversion preserves skip semantics");

        assert!(converted.is_none());
        assert_eq!(budget.remaining_bytes(), 64);
    }

    #[test]
    fn exhausted_aggregate_budget_stops_before_another_conversion() {
        let mut budget = CaptureStringBudget::new(1);
        assert_eq!(
            budget
                .with_conversion_capacity(1, |_| Some("x".to_owned()))
                .expect("first field"),
            Some("x".to_owned())
        );
        let reads = Cell::new(0);
        let error = budget
            .with_conversion_capacity(1, |_| {
                reads.set(reads.get() + 1);
                Some("y".to_owned())
            })
            .expect_err("exhausted budget");

        assert_eq!(reads.get(), 0);
        assert!(matches!(
            error,
            CaptureError::Accessibility(ref message)
                if message == CAPTURE_STRING_BUDGET_EXHAUSTED
        ));
    }

    #[test]
    fn fallback_roles_are_included_in_the_aggregate_budget() {
        let mut budget = CaptureStringBudget::new("AXUnknown".len());
        assert_eq!(
            budget.retain_literal("AXUnknown").expect("exact fit"),
            "AXUnknown"
        );
        assert_eq!(budget.remaining_bytes(), 0);
        assert!(matches!(
            budget.retain_literal("AXUnknown"),
            Err(CaptureError::Accessibility(ref message))
                if message == CAPTURE_STRING_BUDGET_EXHAUSTED
        ));
    }

    #[test]
    fn appkit_metadata_bridge_shares_budget_and_refunds_per_field_overflow() {
        fn cf_string(value: &str) -> OwnedCf {
            let value = CString::new(value).expect("test string");
            OwnedCf::new(unsafe {
                super::CFStringCreateWithCString(ptr::null(), value.as_ptr(), UTF8).cast::<c_void>()
            })
            .expect("Core Foundation string")
        }

        let first = cf_string("a");
        let second = cf_string("b");
        let mut shared_budget = CaptureStringBudget::new(2);
        assert_eq!(
            unsafe { ns_string(first.0.cast_mut(), 64, &mut shared_budget) }
                .expect("first conversion"),
            Some("a".to_owned())
        );
        assert!(matches!(
            unsafe { ns_string(second.0.cast_mut(), 64, &mut shared_budget) },
            Err(CaptureError::Accessibility(ref message))
                if message == CAPTURE_STRING_BUDGET_EXHAUSTED
        ));

        let oversized = cf_string("too long");
        let mut per_field_budget = CaptureStringBudget::new(64);
        assert_eq!(
            unsafe { ns_string(oversized.0.cast_mut(), 1, &mut per_field_budget) }
                .expect("whole-field skip"),
            None
        );
        assert_eq!(per_field_budget.remaining_bytes(), 64);
    }

    #[test]
    fn rejected_traversals_are_zeroized_recursively() {
        let mut node = AccessibilityNode {
            role: "AXWindow".into(),
            value: Some("private capture".into()),
            children: vec![AccessibilityNode {
                role: "AXTextArea".into(),
                title: Some("private title".into()),
                ..AccessibilityNode::default()
            }],
            ..AccessibilityNode::default()
        };
        zeroize_node(&mut node);
        assert!(node.role.is_empty());
        assert!(node.value.is_none());
        assert!(node.children.is_empty());
    }

    #[test]
    fn host_preflight_never_requests_content_like_attributes() {
        let string_reads = RefCell::new(Vec::new());
        let url_reads = RefCell::new(Vec::new());
        let web_area = probe_browser_url_element(
            false,
            |attribute| {
                string_reads.borrow_mut().push(attribute.to_owned());
                match attribute {
                    "AXRole" => Ok(Some("AXWebArea".to_owned())),
                    "AXSubrole" | "AXIdentifier" => Ok(None),
                    "AXTitle" | "AXDescription" | "AXValue" => {
                        panic!("content accessor {attribute} must not be called")
                    }
                    _ => panic!("unexpected string accessor {attribute}"),
                }
            },
            |attribute| {
                url_reads.borrow_mut().push(attribute.to_owned());
                match attribute {
                    "AXDocument" => {
                        Ok(Some(["https", "://", "secret.example.com/report"].concat()))
                    }
                    "AXURL" => Ok(None),
                    _ => panic!("unexpected URL accessor {attribute}"),
                }
            },
        )
        .expect("web metadata probe");
        assert!(matches!(web_area, BrowserUrlElementProbe::Found(_)));
        assert_eq!(
            string_reads.into_inner(),
            ["AXRole", "AXSubrole", "AXIdentifier"]
        );
        assert_eq!(url_reads.into_inner(), ["AXDocument"]);

        let content_reads = RefCell::new(Vec::new());
        let content_node = probe_browser_url_element(
            false,
            |attribute| {
                content_reads.borrow_mut().push(attribute.to_owned());
                match attribute {
                    "AXRole" => Ok(Some("AXStaticText".to_owned())),
                    "AXSubrole" | "AXIdentifier" => Ok(None),
                    "AXTitle" | "AXDescription" | "AXValue" => {
                        panic!("content accessor {attribute} must not be called")
                    }
                    _ => panic!("unexpected string accessor {attribute}"),
                }
            },
            |attribute| panic!("content node URL accessor {attribute} must not be called"),
        )
        .expect("content metadata probe");
        assert_eq!(content_node, BrowserUrlElementProbe::Continue);
        assert_eq!(
            content_reads.into_inner(),
            ["AXRole", "AXSubrole", "AXIdentifier"]
        );
    }

    #[test]
    fn only_web_area_is_treated_as_a_web_document_role() {
        assert!(role_is_web_document("AXWebArea"));
        assert!(role_is_web_document("axwebarea"));
        assert!(!role_is_web_document("AXBrowser"));
        assert!(!role_is_web_document("AXDocument"));
    }

    #[test]
    fn address_value_requires_a_non_content_identifier() {
        let reads = RefCell::new(Vec::new());
        let probe = probe_browser_url_element(
            false,
            |attribute| {
                reads.borrow_mut().push(attribute.to_owned());
                match attribute {
                    "AXRole" => Ok(Some("AXTextField".to_owned())),
                    "AXSubrole" => Ok(None),
                    "AXIdentifier" => Ok(Some("browser-omnibox".to_owned())),
                    "AXValue" => Ok(Some(["https", "://", "example.com/current"].concat())),
                    "AXTitle" | "AXDescription" => {
                        panic!("content accessor {attribute} must not be called")
                    }
                    _ => panic!("unexpected string accessor {attribute}"),
                }
            },
            |attribute| match attribute {
                "AXURL" => Ok(None),
                _ => panic!("unexpected URL accessor {attribute}"),
            },
        )
        .expect("address metadata probe");
        assert!(matches!(probe, BrowserUrlElementProbe::Found(_)));
        assert_eq!(
            reads.into_inner(),
            ["AXRole", "AXSubrole", "AXIdentifier", "AXValue"]
        );
    }

    #[test]
    fn unknown_web_document_never_descends_into_hostile_page_fields() {
        let web_document = probe_browser_url_element(
            false,
            |attribute| match attribute {
                "AXRole" => Ok(Some("AXWebArea".to_owned())),
                "AXSubrole" | "AXIdentifier" => Ok(None),
                "AXTitle" | "AXDescription" | "AXValue" => {
                    panic!("content accessor {attribute} must not be called")
                }
                _ => panic!("unexpected string accessor {attribute}"),
            },
            |attribute| match attribute {
                "AXDocument" | "AXURL" => Ok(None),
                _ => panic!("unexpected URL accessor {attribute}"),
            },
        )
        .expect("web metadata probe");
        assert_eq!(web_document, BrowserUrlElementProbe::WebDocumentWithoutUrl);

        let hostile_child_reads = Cell::new(0);
        let search = resolve_browser_url_subtree(web_document, || {
            hostile_child_reads.set(hostile_child_reads.get() + 1);
            panic!("a web document's page-content children must never be probed")
        })
        .expect("subtree policy");
        assert_eq!(search, BrowserUrlSearch::unknown_web_document());
        assert_eq!(hostile_child_reads.get(), 0);
    }

    #[test]
    fn current_web_document_policy_is_decided_before_content_access() {
        let policy = CapturePolicy::new([BlacklistRule {
            kind: BlacklistKind::BrowserHost,
            pattern: "example.com".to_owned(),
        }]);
        let metadata = metadata();
        let content_reads = Cell::new(0);

        for current_url in [
            Some("https://secret.example.com/payroll"),
            None,
            Some("https://allowed.example/work"),
        ] {
            if web_document_is_authorized(&metadata, &policy, current_url) {
                content_reads.set(content_reads.get() + 1);
            }
        }
        assert_eq!(
            content_reads.get(),
            1,
            "blocked and indeterminate documents must stop before content access"
        );
    }

    #[test]
    fn exhausted_url_metadata_budget_fails_closed_for_url_dependent_rules() {
        let search = resolve_browser_url_subtree(BrowserUrlElementProbe::Continue, || {
            Ok(BrowserUrlSearch::indeterminate())
        })
        .expect("bounded search");
        assert_eq!(search, BrowserUrlSearch::indeterminate());
        for kind in [BlacklistKind::BrowserHost, BlacklistKind::Regex] {
            let policy = CapturePolicy::new([BlacklistRule {
                kind,
                pattern: "example".to_owned(),
            }]);
            assert_eq!(
                finalize_browser_url_preflight(
                    BrowserUrlSearch::indeterminate(),
                    &metadata(),
                    &policy
                ),
                BrowserUrlPreflight::Excluded
            );
            assert!(
                matches!(
                    finalize_browser_url_preflight(
                        BrowserUrlSearch::default(),
                        &metadata(),
                        &policy
                    ),
                    BrowserUrlPreflight::Known(BrowserUrlObservation { ref urls }) if urls.is_empty()
                ),
                "a complete non-browser search may still be captured"
            );
        }
    }

    #[test]
    fn every_visible_web_document_must_pass_preflight() {
        let mut search = BrowserUrlSearch::found("https://allowed.example/work".to_owned());
        search.merge(BrowserUrlSearch::found(
            "https://secret.example.com/payroll".to_owned(),
        ));
        let policy = CapturePolicy::new([BlacklistRule {
            kind: BlacklistKind::BrowserHost,
            pattern: "example.com".to_owned(),
        }]);
        assert_eq!(
            finalize_browser_url_preflight(search, &metadata(), &policy),
            BrowserUrlPreflight::Excluded
        );
    }

    #[test]
    fn changed_browser_or_window_metadata_rejects_the_tree() {
        let metadata = metadata();
        let initial_urls = BrowserUrlObservation {
            urls: vec!["https://allowed.example/one".to_owned()],
        };
        let same_urls = BrowserUrlObservation {
            urls: vec!["https://allowed.example/one".to_owned()],
        };
        assert!(browser_capture_metadata_is_stable(
            &metadata,
            &initial_urls,
            Some("Current page"),
            Some(9_001),
            Some(9_001),
            &same_urls,
        ));

        let navigated_urls = BrowserUrlObservation {
            urls: vec!["https://allowed.example/two".to_owned()],
        };
        assert!(!browser_capture_metadata_is_stable(
            &metadata,
            &initial_urls,
            Some("Current page"),
            Some(9_001),
            Some(9_001),
            &navigated_urls,
        ));
        assert!(!browser_capture_metadata_is_stable(
            &metadata,
            &initial_urls,
            Some("Different page"),
            Some(9_001),
            Some(9_001),
            &same_urls,
        ));
        assert!(!browser_capture_metadata_is_stable(
            &metadata,
            &initial_urls,
            Some("Current page"),
            Some(9_002),
            Some(9_001),
            &same_urls,
        ));
        assert!(browser_capture_metadata_is_stable(
            &metadata,
            &initial_urls,
            Some("Current page"),
            Some(9_002),
            None,
            &same_urls,
        ));
    }

    #[test]
    fn focused_window_and_element_observation_must_remain_exact() {
        assert!(capture_focus_observation_is_stable(
            42,
            42,
            true,
            true,
            Some(9_001),
            Some(9_001),
            Some(9_001),
        ));
        for observation in [
            (7, true, true, Some(9_001), Some(9_001)),
            (42, true, true, Some(9_001), Some(9_002)),
            (42, false, true, Some(9_001), Some(9_001)),
            (42, true, false, Some(9_001), Some(9_001)),
        ] {
            assert!(!capture_focus_observation_is_stable(
                42,
                observation.0,
                observation.1,
                observation.2,
                Some(9_001),
                observation.3,
                observation.4,
            ));
        }
        assert!(capture_focus_observation_is_stable(
            42,
            42,
            true,
            true,
            None,
            None,
            Some(9_002),
        ));
    }
}
