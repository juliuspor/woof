use std::{
    ffi::c_void,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    InlineError, ModifierConfig, ModifierEvent, ModifierInput, ModifierKey, ModifierStateMachine,
};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventMask = u64;
type CGEventFlags = u64;
type CGEventType = u32;

const EVENT_FLAGS_CHANGED: CGEventType = 12;
const EVENT_KEY_DOWN: CGEventType = 10;
const EVENT_TAP_DISABLED_BY_TIMEOUT: CGEventType = u32::MAX - 1;
const EVENT_TAP_DISABLED_BY_USER_INPUT: CGEventType = u32::MAX;
const EVENT_FIELD_KEYCODE: i32 = 9;
const EVENT_SOURCE_COMBINED_SESSION: i32 = 0;
const EVENT_TAP_SESSION: u32 = 1;
const EVENT_TAP_HEAD_INSERT: u32 = 0;
const EVENT_TAP_LISTEN_ONLY: u32 = 1;
const KEY_LEFT_COMMAND: i64 = 55;
const KEY_RIGHT_COMMAND: i64 = 54;
const KEY_LEFT_SHIFT: i64 = 56;
const KEY_RIGHT_SHIFT: i64 = 60;
const KEY_LEFT_OPTION: i64 = 58;
const KEY_RIGHT_OPTION: i64 = 61;
const KEY_LEFT_CONTROL: i64 = 59;
const KEY_RIGHT_CONTROL: i64 = 62;
const KEY_FUNCTION: i64 = 63;
const SHIFT_FLAG: CGEventFlags = 1 << 17;
const CONTROL_FLAG: CGEventFlags = 1 << 18;
const OPTION_FLAG: CGEventFlags = 1 << 19;
const COMMAND_FLAG: CGEventFlags = 1 << 20;
const FUNCTION_FLAG: CGEventFlags = 1 << 23;
const MODIFIER_FLAGS: CGEventFlags =
    SHIFT_FLAG | CONTROL_FLAG | OPTION_FLAG | COMMAND_FLAG | FUNCTION_FLAG;
const POLL_INTERVAL_SECONDS: f64 = 0.01;
const MONITOR_QUEUE_CAPACITY: usize = 64;
const MAX_DISPATCH_PER_POLL: usize = 64;

type EventTapCallback = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
    fn CGRequestListenEventAccess() -> bool;
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: EventTapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: i32) -> i64;
    fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    fn CGEventSourceKeyState(state_id: i32, key: u16) -> bool;
}

/// Returns whether macOS currently allows woof to observe global modifier
/// changes. This is a separate TCC grant from Accessibility.
pub fn input_monitoring_trusted() -> bool {
    // SAFETY: CoreGraphics exposes this as a process-wide, side-effect-free
    // permission query on macOS 10.15 and later.
    unsafe { CGPreflightListenEventAccess() }
}

/// Asks macOS to register/request Input Monitoring access for this signed app.
/// The returned value reflects the state after the request; the user may still
/// need to enable woof in System Settings before it becomes true.
pub fn request_input_monitoring() -> bool {
    // SAFETY: CoreGraphics owns the TCC prompt and returns the resulting grant.
    unsafe { CGRequestListenEventAccess() }
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: CFStringRef;
    static kCFRunLoopCommonModes: CFStringRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(run_loop: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(run_loop: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(
        mode: CFStringRef,
        seconds: f64,
        return_after_source_handled: bool,
    ) -> i32;
    fn CFRelease(value: CFTypeRef);
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn IsSecureEventInputEnabled() -> bool;
}

type CFIndex = isize;
type EventCallback = dyn Fn(ModifierEvent) + Send + Sync + 'static;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedShortcutChord {
    pub meta: bool,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub key: String,
}

#[derive(Clone)]
pub struct ModifierMonitorHandle {
    control: Arc<MonitorControl>,
}

impl ModifierMonitorHandle {
    /// Cancels an active hold-to-talk gesture without stopping the monitor.
    pub fn cancel_active(&self) {
        self.control.cancel.store(true, Ordering::SeqCst);
    }

    /// Requests monitor shutdown. [`ModifierMonitor::stop`] also joins the
    /// native run-loop thread.
    pub fn request_stop(&self) {
        self.control.stop.store(true, Ordering::SeqCst);
    }
}

pub struct ModifierMonitor {
    handle: ModifierMonitorHandle,
    worker: Option<JoinHandle<()>>,
}

impl ModifierMonitor {
    pub fn start<F>(config: ModifierConfig, callback: F) -> Result<Self, InlineError>
    where
        F: Fn(ModifierEvent) + Send + Sync + 'static,
    {
        let control = Arc::new(MonitorControl::default());
        let handle = ModifierMonitorHandle {
            control: Arc::clone(&control),
        };
        let callback: Arc<EventCallback> = Arc::new(callback);
        let (startup_sender, startup_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("woof-inline-modifiers".into())
            .spawn(move || monitor_thread(config, callback, control, startup_sender))
            .map_err(|_| InlineError::EventMonitor)?;
        match startup_receiver.recv() {
            Ok(Ok(())) => Ok(Self {
                handle,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(InlineError::EventMonitor)
            }
        }
    }

    pub fn handle(&self) -> ModifierMonitorHandle {
        self.handle.clone()
    }

    pub fn cancel_active(&self) {
        self.handle.cancel_active();
    }

    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.handle.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for ModifierMonitor {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

/// Records the next supported physical modifier press using the same
/// listen-only event-tap boundary as the background monitor. No key events are
/// swallowed and no event data is logged.
pub fn record_modifier_key(timeout: Duration) -> Result<ModifierKey, InlineError> {
    if unsafe { IsSecureEventInputEnabled() } {
        return Err(InlineError::SecureInput);
    }
    let context = Box::new(ModifierRecordingContext::default());
    let context_pointer = (&*context as *const ModifierRecordingContext)
        .cast_mut()
        .cast();
    let tap = create_recording_tap(
        EVENT_FLAGS_CHANGED,
        record_modifier_callback,
        context_pointer,
    )?;
    let source = create_recording_source(tap)?;
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    unsafe {
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
    }

    let started = Instant::now();
    let result = loop {
        if unsafe { IsSecureEventInputEnabled() } {
            break Err(InlineError::SecureInput);
        }
        if let Some(result) = context
            .result
            .lock()
            .ok()
            .and_then(|mut value| value.take())
        {
            break result;
        }
        if started.elapsed() >= timeout {
            break Err(InlineError::RecordingTimeout);
        }
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, POLL_INTERVAL_SECONDS, true);
        }
    };
    release_recording_tap(run_loop, source, tap);
    result
}

/// Records the next supported non-modifier key-down and its macOS modifiers.
pub fn record_shortcut_chord(timeout: Duration) -> Result<RecordedShortcutChord, InlineError> {
    if unsafe { IsSecureEventInputEnabled() } {
        return Err(InlineError::SecureInput);
    }
    let context = Box::new(ShortcutRecordingContext::default());
    let context_pointer = (&*context as *const ShortcutRecordingContext)
        .cast_mut()
        .cast();
    let tap = create_recording_tap(EVENT_KEY_DOWN, record_shortcut_callback, context_pointer)?;
    let source = create_recording_source(tap)?;
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    unsafe {
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
    }

    let started = Instant::now();
    let result = loop {
        if unsafe { IsSecureEventInputEnabled() } {
            break Err(InlineError::SecureInput);
        }
        if let Some(result) = context
            .result
            .lock()
            .ok()
            .and_then(|mut value| value.take())
        {
            break result;
        }
        if started.elapsed() >= timeout {
            break Err(InlineError::RecordingTimeout);
        }
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, POLL_INTERVAL_SECONDS, true);
        }
    };
    release_recording_tap(run_loop, source, tap);
    result
}

fn create_recording_tap(
    event_type: CGEventType,
    callback: EventTapCallback,
    context: *mut c_void,
) -> Result<CFMachPortRef, InlineError> {
    let tap = unsafe {
        CGEventTapCreate(
            EVENT_TAP_SESSION,
            EVENT_TAP_HEAD_INSERT,
            EVENT_TAP_LISTEN_ONLY,
            1_u64 << event_type,
            callback,
            context,
        )
    };
    if tap.is_null() {
        Err(InlineError::PermissionDenied)
    } else {
        Ok(tap)
    }
}

fn create_recording_source(tap: CFMachPortRef) -> Result<CFRunLoopSourceRef, InlineError> {
    let source = unsafe { CFMachPortCreateRunLoopSource(ptr::null(), tap, 0) };
    if source.is_null() {
        unsafe {
            CFMachPortInvalidate(tap);
            CFRelease(tap.cast());
        }
        Err(InlineError::EventMonitor)
    } else {
        Ok(source)
    }
}

fn release_recording_tap(run_loop: CFRunLoopRef, source: CFRunLoopSourceRef, tap: CFMachPortRef) {
    unsafe {
        CFRunLoopRemoveSource(run_loop, source, kCFRunLoopCommonModes);
        CFMachPortInvalidate(tap);
        CFRelease(source.cast());
        CFRelease(tap.cast());
    }
}

#[derive(Default)]
struct ModifierRecordingContext {
    result: Mutex<Option<Result<ModifierKey, InlineError>>>,
}

#[derive(Default)]
struct ShortcutRecordingContext {
    result: Mutex<Option<Result<RecordedShortcutChord, InlineError>>>,
}

unsafe extern "C" fn record_modifier_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if event_type != EVENT_FLAGS_CHANGED || event.is_null() {
            return;
        }
        let Some(context) = (user_info as *const ModifierRecordingContext).as_ref() else {
            return;
        };
        if IsSecureEventInputEnabled() {
            if let Ok(mut result) = context.result.lock() {
                result.get_or_insert(Err(InlineError::SecureInput));
            }
            return;
        }
        let key_code = CGEventGetIntegerValueField(event, EVENT_FIELD_KEYCODE);
        let Some(key) = modifier_key_from_code(key_code) else {
            return;
        };
        let pressed = modifier_key_is_pressed(event, key_code, key);
        if pressed {
            if IsSecureEventInputEnabled() {
                if let Ok(mut result) = context.result.lock() {
                    result.get_or_insert(Err(InlineError::SecureInput));
                }
                return;
            }
            if let Ok(mut result) = context.result.lock() {
                result.get_or_insert(Ok(key));
            }
        }
    }));
    event
}

unsafe extern "C" fn record_shortcut_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if event_type != EVENT_KEY_DOWN || event.is_null() {
            return;
        }
        let Some(context) = (user_info as *const ShortcutRecordingContext).as_ref() else {
            return;
        };
        if IsSecureEventInputEnabled() {
            if let Ok(mut recorded) = context.result.lock() {
                recorded.get_or_insert(Err(InlineError::SecureInput));
            }
            return;
        }
        let key_code = CGEventGetIntegerValueField(event, EVENT_FIELD_KEYCODE);
        let result = shortcut_key_from_code(key_code)
            .map(|key| {
                let flags = CGEventGetFlags(event);
                RecordedShortcutChord {
                    meta: flags & COMMAND_FLAG != 0,
                    shift: flags & SHIFT_FLAG != 0,
                    alt: flags & OPTION_FLAG != 0,
                    control: flags & CONTROL_FLAG != 0,
                    key: key.to_string(),
                }
            })
            .ok_or(InlineError::UnsupportedRecordingKey);
        if IsSecureEventInputEnabled() {
            if let Ok(mut recorded) = context.result.lock() {
                recorded.get_or_insert(Err(InlineError::SecureInput));
            }
            return;
        }
        if let Ok(mut recorded) = context.result.lock() {
            recorded.get_or_insert(result);
        }
    }));
    event
}

#[derive(Debug, Default)]
struct MonitorControl {
    stop: AtomicBool,
    cancel: AtomicBool,
}

struct MonitorContext {
    sender: mpsc::SyncSender<ModifierInput>,
    started_at: Instant,
    secure_detected: AtomicBool,
    disabled: AtomicBool,
    permission_refused: AtomicBool,
    overflowed: AtomicBool,
}

impl MonitorContext {
    fn new(sender: mpsc::SyncSender<ModifierInput>) -> Self {
        Self {
            sender,
            started_at: Instant::now(),
            secure_detected: AtomicBool::new(false),
            disabled: AtomicBool::new(false),
            permission_refused: AtomicBool::new(false),
            overflowed: AtomicBool::new(false),
        }
    }

    fn enqueue(&self, input: ModifierInput) {
        match self.sender.try_send(input) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                self.overflowed.store(true, Ordering::SeqCst);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.disabled.store(true, Ordering::SeqCst);
            }
        }
    }
}

struct MonitorDispatcher {
    machine: ModifierStateMachine,
    receiver: mpsc::Receiver<ModifierInput>,
    callback: Arc<EventCallback>,
    secure_refused: bool,
    permission_refused: bool,
}

impl MonitorDispatcher {
    fn new(
        config: ModifierConfig,
        receiver: mpsc::Receiver<ModifierInput>,
        callback: Arc<EventCallback>,
    ) -> Self {
        Self {
            machine: ModifierStateMachine::new(config),
            receiver,
            callback,
            secure_refused: false,
            permission_refused: false,
        }
    }

    fn emit(&self, events: impl IntoIterator<Item = ModifierEvent>) {
        for event in events {
            let _ = catch_unwind(AssertUnwindSafe(|| (self.callback)(event)));
        }
    }

    fn cancel_mut(&mut self) {
        let events = self.machine.cancel();
        self.emit(events);
    }

    fn refuse_secure_input(&mut self) {
        self.cancel_mut();
        self.discard_pending();
        if !self.secure_refused {
            self.secure_refused = true;
            self.emit([ModifierEvent::SecureInputRefused]);
        }
    }

    fn refuse_permission(&mut self) {
        self.cancel_mut();
        self.discard_pending();
        if !self.permission_refused {
            self.permission_refused = true;
            self.emit([ModifierEvent::PermissionRefused]);
        }
    }

    fn reset_refusals(&mut self) {
        self.secure_refused = false;
        self.permission_refused = false;
    }

    fn dispatch_pending(&mut self) {
        for _ in 0..MAX_DISPATCH_PER_POLL {
            match self.receiver.try_recv() {
                Ok(input) => {
                    let events = self.machine.handle(input);
                    self.emit(events);
                }
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    fn discard_pending(&self) {
        while self.receiver.try_recv().is_ok() {}
    }

    fn poll(&mut self, now: Duration) {
        let events = self.machine.poll(now);
        self.emit(events);
    }
}

fn monitor_thread(
    config: ModifierConfig,
    callback: Arc<EventCallback>,
    control: Arc<MonitorControl>,
    startup: mpsc::SyncSender<Result<(), InlineError>>,
) {
    let (signal_sender, signal_receiver) = mpsc::sync_channel(MONITOR_QUEUE_CAPACITY);
    let context = Box::new(MonitorContext::new(signal_sender));
    let mut dispatcher = MonitorDispatcher::new(config, signal_receiver, callback);
    let context_pointer = (&*context as *const MonitorContext).cast_mut().cast();
    let event_mask = 1_u64 << EVENT_FLAGS_CHANGED;
    // SAFETY: The context remains boxed and stable until the tap/source have
    // been removed and invalidated below.
    let tap = unsafe {
        CGEventTapCreate(
            EVENT_TAP_SESSION,
            EVENT_TAP_HEAD_INSERT,
            EVENT_TAP_LISTEN_ONLY,
            event_mask,
            event_tap_callback,
            context_pointer,
        )
    };
    if tap.is_null() {
        let _ = startup.send(Err(InlineError::PermissionDenied));
        return;
    }
    let source = unsafe { CFMachPortCreateRunLoopSource(ptr::null(), tap, 0) };
    if source.is_null() {
        unsafe {
            CFMachPortInvalidate(tap);
            CFRelease(tap.cast());
        }
        let _ = startup.send(Err(InlineError::EventMonitor));
        return;
    }
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    unsafe {
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
    }
    if startup.send(Ok(())).is_err() {
        control.stop.store(true, Ordering::SeqCst);
    }

    while !control.stop.load(Ordering::SeqCst) {
        if control.cancel.swap(false, Ordering::SeqCst) {
            dispatcher.cancel_mut();
        }
        if context.disabled.swap(false, Ordering::SeqCst) {
            dispatcher.cancel_mut();
            unsafe { CGEventTapEnable(tap, true) };
        }
        if context.permission_refused.swap(false, Ordering::SeqCst) {
            dispatcher.refuse_permission();
        }
        // SAFETY: Carbon reads process-global secure-input state. Any event
        // callback observation is latched so the dispatcher refuses even if
        // secure input toggles again before this loop iteration.
        let secure_input = unsafe { IsSecureEventInputEnabled() }
            || context.secure_detected.swap(false, Ordering::SeqCst);
        if secure_input {
            dispatcher.refuse_secure_input();
        } else {
            dispatcher.reset_refusals();
            if context.overflowed.swap(false, Ordering::SeqCst) {
                dispatcher.cancel_mut();
                dispatcher.discard_pending();
            }
            dispatcher.dispatch_pending();
            dispatcher.poll(context.started_at.elapsed());
        }
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, POLL_INTERVAL_SECONDS, true);
        }
    }

    dispatcher.cancel_mut();
    unsafe {
        CFRunLoopRemoveSource(run_loop, source, kCFRunLoopCommonModes);
        CFMachPortInvalidate(tap);
        CFRelease(source.cast());
        CFRelease(tap.cast());
    }
    drop(context);
}

unsafe extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(context) = (user_info as *const MonitorContext).as_ref() else {
            return;
        };
        if matches!(
            event_type,
            EVENT_TAP_DISABLED_BY_TIMEOUT | EVENT_TAP_DISABLED_BY_USER_INPUT
        ) {
            context.disabled.store(true, Ordering::SeqCst);
            if !CGPreflightListenEventAccess() {
                context.permission_refused.store(true, Ordering::SeqCst);
            }
            return;
        }
        if event_type != EVENT_FLAGS_CHANGED || event.is_null() {
            return;
        }
        if IsSecureEventInputEnabled() {
            context.secure_detected.store(true, Ordering::SeqCst);
            return;
        }

        let key_code = CGEventGetIntegerValueField(event, EVENT_FIELD_KEYCODE);
        let Some(key) = modifier_key_from_code(key_code) else {
            return;
        };
        let pressed = modifier_key_is_pressed(event, key_code, key);
        let flags = CGEventGetFlags(event);
        let other_modifiers = flags & (MODIFIER_FLAGS & !modifier_flag(key)) != 0;
        let input = ModifierInput {
            key,
            pressed,
            other_modifiers,
            at: context.started_at.elapsed(),
        };
        if IsSecureEventInputEnabled() {
            context.secure_detected.store(true, Ordering::SeqCst);
            return;
        }
        context.enqueue(input);
    }));
    event
}

fn modifier_key_from_code(key_code: i64) -> Option<ModifierKey> {
    match key_code {
        KEY_FUNCTION => Some(ModifierKey::Fn),
        KEY_LEFT_OPTION => Some(ModifierKey::LeftOption),
        KEY_RIGHT_OPTION => Some(ModifierKey::RightOption),
        KEY_LEFT_COMMAND => Some(ModifierKey::LeftCommand),
        KEY_RIGHT_COMMAND => Some(ModifierKey::RightCommand),
        KEY_LEFT_SHIFT => Some(ModifierKey::LeftShift),
        KEY_RIGHT_SHIFT => Some(ModifierKey::RightShift),
        KEY_LEFT_CONTROL => Some(ModifierKey::LeftControl),
        KEY_RIGHT_CONTROL => Some(ModifierKey::RightControl),
        _ => None,
    }
}

fn modifier_key_is_pressed(event: CGEventRef, key_code: i64, key: ModifierKey) -> bool {
    if key == ModifierKey::Fn {
        // Globe/Fn is represented by the function modifier flag on Apple
        // keyboards and is not consistently reflected by key-state queries.
        unsafe { CGEventGetFlags(event) & FUNCTION_FLAG != 0 }
    } else {
        unsafe {
            CGEventSourceKeyState(
                EVENT_SOURCE_COMBINED_SESSION,
                u16::try_from(key_code).unwrap_or_default(),
            )
        }
    }
}

const fn modifier_flag(key: ModifierKey) -> CGEventFlags {
    match key {
        ModifierKey::Fn => FUNCTION_FLAG,
        ModifierKey::LeftOption | ModifierKey::RightOption => OPTION_FLAG,
        ModifierKey::LeftCommand | ModifierKey::RightCommand => COMMAND_FLAG,
        ModifierKey::LeftShift | ModifierKey::RightShift => SHIFT_FLAG,
        ModifierKey::LeftControl | ModifierKey::RightControl => CONTROL_FLAG,
    }
}

fn shortcut_key_from_code(key_code: i64) -> Option<&'static str> {
    // ANSI key codes are layout-stable physical keys. The returned spelling is
    // accepted by global-hotkey and matches woof's lowercase chord JSON.
    match key_code {
        0 => Some("a"),
        1 => Some("s"),
        2 => Some("d"),
        3 => Some("f"),
        4 => Some("h"),
        5 => Some("g"),
        6 => Some("z"),
        7 => Some("x"),
        8 => Some("c"),
        9 => Some("v"),
        11 => Some("b"),
        12 => Some("q"),
        13 => Some("w"),
        14 => Some("e"),
        15 => Some("r"),
        16 => Some("y"),
        17 => Some("t"),
        18 => Some("1"),
        19 => Some("2"),
        20 => Some("3"),
        21 => Some("4"),
        22 => Some("6"),
        23 => Some("5"),
        25 => Some("9"),
        26 => Some("7"),
        28 => Some("8"),
        29 => Some("0"),
        31 => Some("o"),
        32 => Some("u"),
        34 => Some("i"),
        35 => Some("p"),
        37 => Some("l"),
        38 => Some("j"),
        40 => Some("k"),
        45 => Some("n"),
        46 => Some("m"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_tap_queue_defers_user_callbacks_to_the_dispatcher() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let callback_events = Arc::clone(&events);
        let callback: Arc<EventCallback> = Arc::new(move |event| {
            callback_events.lock().unwrap().push(event);
        });
        let (sender, receiver) = mpsc::sync_channel(MONITOR_QUEUE_CAPACITY);
        let context = MonitorContext::new(sender);
        let mut dispatcher = MonitorDispatcher::new(ModifierConfig::default(), receiver, callback);
        for (pressed, at) in [(true, 0), (false, 50), (true, 120), (false, 170)] {
            context.enqueue(ModifierInput {
                key: ModifierKey::RightOption,
                pressed,
                other_modifiers: false,
                at: Duration::from_millis(at),
            });
        }
        assert!(events.lock().unwrap().is_empty());
        dispatcher.dispatch_pending();
        assert_eq!(*events.lock().unwrap(), vec![ModifierEvent::InlineInvoked]);
    }

    #[test]
    fn maps_every_supported_modifier_key_code() {
        let expected = [
            (KEY_FUNCTION, ModifierKey::Fn),
            (KEY_LEFT_OPTION, ModifierKey::LeftOption),
            (KEY_RIGHT_OPTION, ModifierKey::RightOption),
            (KEY_LEFT_COMMAND, ModifierKey::LeftCommand),
            (KEY_RIGHT_COMMAND, ModifierKey::RightCommand),
            (KEY_LEFT_SHIFT, ModifierKey::LeftShift),
            (KEY_RIGHT_SHIFT, ModifierKey::RightShift),
            (KEY_LEFT_CONTROL, ModifierKey::LeftControl),
            (KEY_RIGHT_CONTROL, ModifierKey::RightControl),
        ];
        for (code, key) in expected {
            assert_eq!(modifier_key_from_code(code), Some(key));
        }
    }

    #[test]
    fn shortcut_recording_uses_the_canonical_key_shape() {
        assert_eq!(shortcut_key_from_code(5), Some("g"));
        assert_eq!(shortcut_key_from_code(49), None);
        assert_eq!(shortcut_key_from_code(KEY_LEFT_OPTION), None);
    }
}
