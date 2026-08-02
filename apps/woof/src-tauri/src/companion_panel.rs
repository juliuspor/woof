use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::Duration,
};

#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicU8;

use serde::{Deserialize, Serialize};
use tauri::{
    LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize, Position, Size, WebviewWindow,
};

#[cfg(target_os = "macos")]
use objc2::{
    define_class,
    ffi::{objc_getAssociatedObject, objc_setAssociatedObject, OBJC_ASSOCIATION_RETAIN_NONATOMIC},
    msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObjectProtocol},
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSEvent, NSTrackingArea, NSTrackingAreaOptions, NSWindow};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSObject, NSPoint, NSRect, NSSize};
#[cfg(target_os = "macos")]
use tauri::Emitter;

pub const WINDOW_LABEL: &str = "companion-chat";
pub const POINTER_EVENT: &str = "woof:companion-pointer";
pub const COLLAPSED_WIDTH: f64 = 260.0;
pub const COLLAPSED_HEIGHT: f64 = 32.0;
pub const EXPANDED_WIDTH: f64 = 588.0;
pub const EXPANDED_HEIGHT: f64 = 440.0;

pub const DEFAULT_MORPH_DURATION_S: f64 = 0.12;
const MAX_ANIMATION_DURATION_S: f64 = 2.0;
static ALPHA_GENERATION: AtomicU64 = AtomicU64::new(0);
static DRAG_MODE: Mutex<Option<PanelMode>> = Mutex::new(None);

#[cfg(target_os = "macos")]
static HOVER_TRACKER_ASSOCIATION_KEY: u8 = 0;

#[cfg(target_os = "macos")]
struct HoverTrackingIvars {
    window: WebviewWindow,
    native_window: usize,
    pointer_state: AtomicU8,
}

#[cfg(target_os = "macos")]
define_class!(
    #[unsafe(super = NSObject)]
    #[name = "WoofCompanionHoverTrackingOwner"]
    #[thread_kind = MainThreadOnly]
    #[ivars = HoverTrackingIvars]
    struct HoverTrackingOwner;

    unsafe impl NSObjectProtocol for HoverTrackingOwner {}

    impl HoverTrackingOwner {
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &NSEvent) {
            self.publish_pointer_truth();
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _event: &NSEvent) {
            self.publish_pointer_truth();
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, _event: &NSEvent) {
            self.publish_pointer_truth();
        }
    }
);

#[cfg(target_os = "macos")]
impl HoverTrackingOwner {
    fn new(window: WebviewWindow, native_window: usize, mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(HoverTrackingIvars {
            window,
            native_window,
            pointer_state: AtomicU8::new(0),
        });
        // SAFETY: NSObject's init signature is correct and the ivars were set.
        unsafe { msg_send![super(this), init] }
    }

    fn publish_pointer_truth(&self) {
        // SAFETY: Tauri supplied the NSWindow pointer retained by the
        // WebviewWindow ivar, and AppKit invokes tracking callbacks on its main
        // thread.
        let Some(native) = (unsafe { (self.ivars().native_window as *mut NSWindow).as_ref() })
        else {
            return;
        };
        let inside = native_pointer_inside(native);
        let next = if inside { 2 } else { 1 };
        if self.ivars().pointer_state.swap(next, Ordering::AcqRel) == next {
            return;
        }
        let _ = self.ivars().window.emit(POINTER_EVENT, inside);
    }
}

#[cfg(target_os = "macos")]
fn native_pointer_inside(native: &NSWindow) -> bool {
    native.contentView().is_some_and(|content| {
        let point = content.convertPoint_fromView(native.mouseLocationOutsideOfEventStream(), None);
        let bounds = content.bounds();
        native.isVisible()
            && point.x >= bounds.origin.x
            && point.y >= bounds.origin.y
            && point.x < bounds.origin.x + bounds.size.width
            && point.y < bounds.origin.y + bounds.size.height
    })
}

/// Dock positions supported by the woof companion window and its Tauri API.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockPosition {
    #[default]
    Top,
    Left,
    Right,
    Bottom,
    BottomLeft,
    BottomRight,
}

impl DockPosition {
    #[cfg(test)]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Left => "left",
            Self::Right => "right",
            Self::Bottom => "bottom",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanelMode {
    Collapsed,
    Expanded,
}

impl PanelMode {
    pub fn from_state(state: &str) -> Result<Self, String> {
        match state {
            "hidden" | "collapsed" => Ok(Self::Collapsed),
            "expanded" => Ok(Self::Expanded),
            _ => Err("invalid companion state".into()),
        }
    }

    fn logical_size(self, dock: DockPosition) -> LogicalSize<f64> {
        match self {
            Self::Collapsed => match dock {
                DockPosition::Top | DockPosition::Bottom => {
                    LogicalSize::new(COLLAPSED_WIDTH, COLLAPSED_HEIGHT)
                }
                DockPosition::Left | DockPosition::Right => {
                    LogicalSize::new(COLLAPSED_HEIGHT, COLLAPSED_WIDTH)
                }
                DockPosition::BottomLeft | DockPosition::BottomRight => {
                    LogicalSize::new(COLLAPSED_HEIGHT, COLLAPSED_HEIGHT)
                }
            },
            Self::Expanded => LogicalSize::new(EXPANDED_WIDTH, EXPANDED_HEIGHT),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PhysicalMonitor {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalFrame {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl PhysicalMonitor {
    fn from_tauri(monitor: &tauri::Monitor) -> Self {
        let origin = monitor.position();
        let size = monitor.size();
        Self {
            x: origin.x,
            y: origin.y,
            width: size.width,
            height: size.height,
            scale_factor: normalized_scale_factor(monitor.scale_factor()),
        }
    }
}

/// Returns a top-edge frame in Tauri's physical coordinate space.
///
/// The dimensions are logical points multiplied by the destination monitor's
/// scale factor. Centering is performed after conversion so odd-sized and
/// non-primary monitors stay pixel-exact instead of accumulating point-space
/// rounding error.
#[cfg(test)]
fn physical_frame_for_monitor(monitor: PhysicalMonitor, mode: PanelMode) -> PhysicalFrame {
    physical_frame_for_monitor_at(monitor, mode, DockPosition::Top)
}

pub fn physical_frame_for_monitor_at(
    monitor: PhysicalMonitor,
    mode: PanelMode,
    dock: DockPosition,
) -> PhysicalFrame {
    let logical = mode.logical_size(dock);
    let width = scale_dimension(logical.width, monitor.scale_factor);
    let height = scale_dimension(logical.height, monitor.scale_factor);
    let horizontal_center = (i64::from(monitor.width) - i64::from(width)).div_euclid(2);
    let vertical_center = (i64::from(monitor.height) - i64::from(height)).div_euclid(2);
    let left = i64::from(monitor.x);
    let top = i64::from(monitor.y);
    let right = left + i64::from(monitor.width) - i64::from(width);
    let bottom = top + i64::from(monitor.height) - i64::from(height);
    let (x, y) = match dock {
        DockPosition::Top => (left + horizontal_center, top),
        DockPosition::Left => (left, top + vertical_center),
        DockPosition::Right => (right, top + vertical_center),
        DockPosition::Bottom => (left + horizontal_center, bottom),
        DockPosition::BottomLeft => (left, bottom),
        DockPosition::BottomRight => (right, bottom),
    };

    PhysicalFrame {
        x: clamp_i32(x),
        y: clamp_i32(y),
        width,
        height,
    }
}

fn clamp_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn scale_dimension(logical: f64, scale_factor: f64) -> u32 {
    let scale_factor = normalized_scale_factor(scale_factor);
    (logical * scale_factor).round().clamp(1.0, u32::MAX as f64) as u32
}

fn normalized_scale_factor(scale_factor: f64) -> f64 {
    if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    }
}

fn destination_monitor(window: &WebviewWindow) -> Result<tauri::Monitor, String> {
    window
        .current_monitor()
        .map_err(|_| "could not inspect the companion monitor")?
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available for the companion".to_string())
}

pub fn set_mode_at(
    window: &WebviewWindow,
    mode: PanelMode,
    dock: DockPosition,
    animated: bool,
) -> Result<PhysicalFrame, String> {
    let duration_s = animated.then_some(DEFAULT_MORPH_DURATION_S);
    set_mode_timed_at(window, mode, dock, duration_s)
}

pub fn set_mode_timed_at(
    window: &WebviewWindow,
    mode: PanelMode,
    dock: DockPosition,
    duration_s: Option<f64>,
) -> Result<PhysicalFrame, String> {
    let duration_s = normalized_animation_duration(duration_s);
    let monitor = destination_monitor(window)?;
    let physical_monitor = PhysicalMonitor::from_tauri(&monitor);
    let frame = physical_frame_for_monitor_at(physical_monitor, mode, dock);

    // Startup and display-change corrections must land before a hidden
    // window is shown. AppKit's main-thread frame call below is scheduled by
    // Tauri, so establish the same tested frame synchronously when no morph
    // animation is requested to prevent a one-frame centered flash.
    if duration_s == 0.0 {
        window
            .set_size(Size::Physical(PhysicalSize::new(frame.width, frame.height)))
            .map_err(|_| "could not resize the companion")?;
        window
            .set_position(Position::Physical(PhysicalPosition::new(frame.x, frame.y)))
            .map_err(|_| "could not dock the companion")?;
    }

    #[cfg(target_os = "macos")]
    {
        set_native_frame(window, physical_monitor, frame, duration_s)?;
        Ok(frame)
    }

    #[cfg(not(target_os = "macos"))]
    {
        // The non-macOS implementation uses an already-computed final frame,
        // so it cannot drift from the tested geometry.
        window
            .set_size(Size::Physical(PhysicalSize::new(frame.width, frame.height)))
            .map_err(|_| "could not resize the companion")?;
        window
            .set_position(Position::Physical(PhysicalPosition::new(frame.x, frame.y)))
            .map_err(|_| "could not dock the companion")?;
        Ok(frame)
    }
}

fn normalized_animation_duration(duration_s: Option<f64>) -> f64 {
    match duration_s {
        Some(duration) if duration.is_finite() && duration > 0.0 => {
            duration.min(MAX_ANIMATION_DURATION_S)
        }
        _ => 0.0,
    }
}

/// Animates the persistent AppKit window's opacity. Fade-in makes the window
/// visible before beginning; fade-out hides it after the animation finishes.
/// A generation guard prevents a stale fade-out timer from hiding a panel that
/// was shown again before the previous transition completed.
pub fn set_alpha(window: &WebviewWindow, alpha: f64, duration_s: f64) -> Result<(), String> {
    let target = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let duration_s = normalized_animation_duration(Some(duration_s));
    let generation = ALPHA_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    if target > 0.0 {
        window.show().map_err(|_| "could not show the companion")?;
    }

    #[cfg(target_os = "macos")]
    set_native_alpha(window, target, duration_s)?;

    if target <= 0.0 {
        if duration_s == 0.0 {
            window.hide().map_err(|_| "could not hide the companion")?;
        } else {
            let window = window.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs_f64(duration_s));
                if ALPHA_GENERATION.load(Ordering::Acquire) == generation {
                    let _ = window.hide();
                }
            });
        }
    }

    Ok(())
}

pub fn configure_at(window: &WebviewWindow, dock: DockPosition) -> Result<PhysicalFrame, String> {
    set_mode_at(window, PanelMode::Collapsed, dock, false)
}

/// Installs a native AppKit tracking area on the persistent companion panel.
///
/// WKWebView DOM enter/leave events are not reliable while this borderless,
/// non-activating window sits above another application. AppKit tracking is
/// active regardless of which application is frontmost and publishes the
/// cursor truth back to the companion webview.
pub fn install_hover_tracking(window: &WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        install_native_hover_tracking(window)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        Ok(())
    }
}

/// Forces one native pointer-truth event after the frontend listener is ready.
///
/// Returning a boolean from an async command would allow an older snapshot to
/// arrive after a newer tracking event. Emitting from AppKit's main thread
/// preserves the same ordering as the regular tracking callbacks.
pub fn publish_pointer_snapshot(window: &WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        publish_native_pointer_snapshot(window)
    }

    #[cfg(not(target_os = "macos"))]
    {
        window
            .emit(POINTER_EVENT, false)
            .map_err(|_| "could not publish companion pointer state".to_string())
    }
}

pub fn redock_current_mode_at(
    window: &WebviewWindow,
    dock: DockPosition,
) -> Result<PhysicalFrame, String> {
    let mode = current_mode(window);
    set_mode_at(window, mode, dock, false)
}

pub fn begin_drag(window: &WebviewWindow) -> Result<PanelMode, String> {
    let mode = current_mode(window);
    *DRAG_MODE
        .lock()
        .map_err(|_| "companion drag state is unavailable".to_string())? = Some(mode);
    Ok(mode)
}

pub fn set_drag_frame(
    window: &WebviewWindow,
    x: f64,
    y_from_top: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if ![x, y_from_top, width, height]
        .into_iter()
        .all(f64::is_finite)
    {
        return Err("invalid companion drag frame".into());
    }
    let width = width.clamp(80.0, 1_200.0);
    let height = height.clamp(24.0, 1_000.0);
    window
        .set_size(Size::Logical(LogicalSize::new(width, height)))
        .map_err(|_| "could not resize the companion drag preview")?;
    window
        .set_position(Position::Logical(LogicalPosition::new(x, y_from_top)))
        .map_err(|_| "could not move the companion drag preview".to_string())
}

pub fn finish_drag(window: &WebviewWindow) -> Result<(DockPosition, PanelMode), String> {
    let mode = DRAG_MODE
        .lock()
        .map_err(|_| "companion drag state is unavailable".to_string())?
        .take()
        .unwrap_or_else(|| current_mode(window));
    Ok((drag_nearest(window)?, mode))
}

/// Returns the dock target nearest the current drag preview without consuming
/// the saved pre-drag panel mode. Native code uses this for the
/// `position-drag` preview event while pointer frames are still arriving.
pub fn drag_nearest(window: &WebviewWindow) -> Result<DockPosition, String> {
    let monitor = destination_monitor(window)?;
    let monitor = PhysicalMonitor::from_tauri(&monitor);
    let position = window
        .outer_position()
        .map_err(|_| "could not inspect the companion drag position")?;
    let size = window
        .outer_size()
        .map_err(|_| "could not inspect the companion drag size")?;
    Ok(nearest_dock_for_frame(
        monitor,
        PhysicalFrame {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        },
    ))
}

fn current_mode(window: &WebviewWindow) -> PanelMode {
    let scale = window.scale_factor().unwrap_or(1.0);
    let (logical_width, logical_height) = window
        .outer_size()
        .map(|size| {
            let scale = normalized_scale_factor(scale);
            (
                f64::from(size.width) / scale,
                f64::from(size.height) / scale,
            )
        })
        .unwrap_or((COLLAPSED_WIDTH, COLLAPSED_HEIGHT));
    mode_for_logical_size(logical_width, logical_height)
}

fn mode_for_logical_size(width: f64, height: f64) -> PanelMode {
    // Every collapsed form fits inside a 260×260 square: top/bottom
    // are 260×32, sides are 32×260, and corners are 32×32. Expanded chat is
    // larger on both axes. A little tolerance prevents one-pixel backing-scale
    // rounding from turning a side tab into an expanded panel during drag.
    if width > COLLAPSED_WIDTH + 2.0 || height > COLLAPSED_WIDTH + 2.0 {
        PanelMode::Expanded
    } else {
        PanelMode::Collapsed
    }
}

#[cfg(target_os = "macos")]
fn install_native_hover_tracking(window: &WebviewWindow) -> Result<(), String> {
    let raw_window = window
        .ns_window()
        .map_err(|_| "could not access the native companion window")? as usize;
    let keepalive = window.clone();
    let apply = move || -> Result<(), String> {
        let window = keepalive;
        // SAFETY: Tauri supplied this pointer for the retained WebviewWindow.
        // The closure is dispatched to AppKit's main thread before it is run.
        let Some(native) = (unsafe { (raw_window as *mut NSWindow).as_ref() }) else {
            return Err("the native companion window is unavailable".into());
        };
        let Some(content) = native.contentView() else {
            return Err("the native companion content view is unavailable".into());
        };
        let content_ptr = Retained::as_ptr(&content).cast::<AnyObject>();
        let association_key =
            std::ptr::addr_of!(HOVER_TRACKER_ASSOCIATION_KEY).cast::<std::ffi::c_void>();

        // Reconfiguration can run more than once over the lifetime of the
        // persistent panel. Keep exactly one native tracker attached.
        // SAFETY: Both pointers are valid Objective-C runtime objects/keys for
        // the lifetime of the application.
        if !(unsafe { objc_getAssociatedObject(content_ptr, association_key) }).is_null() {
            return Ok(());
        }

        let mtm = MainThreadMarker::new()
            .ok_or_else(|| "companion hover tracking requires AppKit's main thread".to_string())?;
        let owner = HoverTrackingOwner::new(window, raw_window, mtm);
        let options = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect
            | NSTrackingAreaOptions::EnabledDuringMouseDrag;
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
        // SAFETY: HoverTrackingOwner implements the mouse selectors required
        // by these options and the visible-rect option keeps geometry current
        // while the companion morphs between collapsed and expanded frames.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                rect,
                options,
                Some(owner.as_ref()),
                None,
            )
        };
        content.addTrackingArea(&area);
        native.setAcceptsMouseMovedEvents(true);

        // NSTrackingArea does not own its event owner. Associate the owner with
        // the tracked view so it lives exactly as long as that native view.
        // SAFETY: OBJC_ASSOCIATION_RETAIN_NONATOMIC retains `owner`; all access
        // and teardown occur on AppKit's main thread.
        unsafe {
            objc_setAssociatedObject(
                content_ptr.cast_mut(),
                association_key,
                Retained::as_ptr(&owner).cast_mut().cast::<AnyObject>(),
                OBJC_ASSOCIATION_RETAIN_NONATOMIC,
            );
        }
        Ok(())
    };

    if MainThreadMarker::new().is_some() {
        return apply();
    }

    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let _ = result_tx.send(apply());
        })
        .map_err(|_| "could not schedule companion hover tracking".to_string())?;
    result_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "companion hover tracking timed out".to_string())?
}

#[cfg(target_os = "macos")]
fn publish_native_pointer_snapshot(window: &WebviewWindow) -> Result<(), String> {
    let raw_window = window
        .ns_window()
        .map_err(|_| "could not access the native companion window")? as usize;
    let keepalive = window.clone();
    let publish = move || -> Result<(), String> {
        // SAFETY: Tauri supplied this pointer for the retained WebviewWindow.
        // The closure only dereferences it on AppKit's main thread.
        let Some(native) = (unsafe { (raw_window as *mut NSWindow).as_ref() }) else {
            return Err("the native companion window is unavailable".into());
        };
        keepalive
            .emit(POINTER_EVENT, native_pointer_inside(native))
            .map_err(|_| "could not publish companion pointer state".to_string())
    };

    if MainThreadMarker::new().is_some() {
        return publish();
    }

    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let _ = result_tx.send(publish());
        })
        .map_err(|_| "could not schedule companion pointer snapshot".to_string())?;
    result_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "companion pointer snapshot timed out".to_string())?
}

/// Chooses the same six-position edge vocabulary from a dragged window's
/// center. Bottom corners win only in the outer third of the display; the
/// middle third maps to the centered bottom position.
pub fn nearest_dock_for_frame(monitor: PhysicalMonitor, frame: PhysicalFrame) -> DockPosition {
    let center_x = i64::from(frame.x) + i64::from(frame.width) / 2;
    let center_y = i64::from(frame.y) + i64::from(frame.height) / 2;
    let left = i64::from(monitor.x);
    let top = i64::from(monitor.y);
    let right = left + i64::from(monitor.width);
    let bottom = top + i64::from(monitor.height);
    let distance_left = center_x.saturating_sub(left).abs();
    let distance_right = right.saturating_sub(center_x).abs();
    let distance_top = center_y.saturating_sub(top).abs();
    let distance_bottom = bottom.saturating_sub(center_y).abs();

    if distance_top <= distance_left.min(distance_right).min(distance_bottom) {
        return DockPosition::Top;
    }
    if distance_bottom <= distance_left.min(distance_right) {
        let relative_x = center_x.saturating_sub(left);
        let third = i64::from(monitor.width) / 3;
        return if relative_x < third {
            DockPosition::BottomLeft
        } else if relative_x > i64::from(monitor.width) - third {
            DockPosition::BottomRight
        } else {
            DockPosition::Bottom
        };
    }
    if distance_left <= distance_right {
        DockPosition::Left
    } else {
        DockPosition::Right
    }
}

#[cfg(target_os = "macos")]
fn set_native_frame(
    window: &WebviewWindow,
    monitor: PhysicalMonitor,
    target: PhysicalFrame,
    duration_s: f64,
) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSAnimatablePropertyContainer, NSAnimationContext, NSScreen, NSStatusWindowLevel, NSView,
        NSWindow,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    let raw_window = window
        .ns_window()
        .map_err(|_| "could not access the native companion window")? as usize;
    let raw_view = window
        .ns_view()
        .map_err(|_| "could not access the native companion content view")?
        as usize;
    let keepalive = window.clone();
    let apply = move || -> Result<(), String> {
        let _keepalive = keepalive;
        // SAFETY: Tauri supplied this NSWindow pointer for the retained
        // WebviewWindow above. This closure runs only on AppKit's main thread,
        // and the retained Tauri window outlives every use here.
        let Some(native) = (unsafe { (raw_window as *mut NSWindow).as_ref() }) else {
            return Err("the native companion window is unavailable".into());
        };
        let screen = native
            .screen()
            .or_else(|| MainThreadMarker::new().and_then(NSScreen::mainScreen));
        let Some(screen) = screen else {
            return Err("the companion screen is unavailable".into());
        };
        // SAFETY: Tauri supplied this NSView pointer for the retained window.
        let Some(content) = (unsafe { (raw_view as *mut NSView).as_ref() }) else {
            return Err("the native companion content view is unavailable".into());
        };

        let screen_frame = screen.frame();
        let scale = monitor.scale_factor;
        let local_x = (i64::from(target.x) - i64::from(monitor.x)) as f64 / scale;
        let top_offset = (i64::from(target.y) - i64::from(monitor.y)) as f64 / scale;
        let width = f64::from(target.width) / scale;
        let height = f64::from(target.height) / scale;
        let appkit_frame = NSRect::new(
            NSPoint::new(
                screen_frame.origin.x + local_x,
                screen_frame.origin.y + screen_frame.size.height - top_offset - height,
            ),
            NSSize::new(width, height),
        );
        let content_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, height));

        native.setLevel(NSStatusWindowLevel);
        if duration_s > 0.0 {
            NSAnimationContext::beginGrouping();
            let context = NSAnimationContext::currentContext();
            context.setDuration(duration_s);
            context.setAllowsImplicitAnimation(true);
            native.animator().setFrame_display(appkit_frame, true);
            content.animator().setFrame(content_frame);
            NSAnimationContext::endGrouping();
        } else {
            native.setFrame_display(appkit_frame, true);
            content.setFrame(content_frame);
        }

        // Directly morphing NSWindow bypasses Tao's usual resize path. Keep
        // WKWebView and the tagged NSVisualEffectView on the same final bounds.
        content.setNeedsLayout(true);
        content.layoutSubtreeIfNeeded();
        Ok(())
    };

    if MainThreadMarker::new().is_some() {
        return apply();
    }

    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let _ = result_tx.send(apply());
        })
        .map_err(|_| "could not schedule the companion frame update".to_string())?;
    result_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "the companion frame update timed out".to_string())?
}

#[cfg(target_os = "macos")]
fn set_native_alpha(window: &WebviewWindow, target: f64, duration_s: f64) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAnimatablePropertyContainer, NSAnimationContext, NSWindow};

    let raw_window = window
        .ns_window()
        .map_err(|_| "could not access the native companion window")? as usize;
    let keepalive = window.clone();
    let apply = move || -> Result<(), String> {
        let _keepalive = keepalive;
        // SAFETY: Tauri supplied this pointer for the retained window and this
        // closure only dereferences it on AppKit's main thread.
        let Some(native) = (unsafe { (raw_window as *mut NSWindow).as_ref() }) else {
            return Err("the native companion window is unavailable".into());
        };

        if duration_s > 0.0 {
            NSAnimationContext::beginGrouping();
            let context = NSAnimationContext::currentContext();
            context.setDuration(duration_s);
            context.setAllowsImplicitAnimation(true);
            native.animator().setAlphaValue(target);
            NSAnimationContext::endGrouping();
        } else {
            native.setAlphaValue(target);
        }
        Ok(())
    };

    if MainThreadMarker::new().is_some() {
        return apply();
    }

    let (result_tx, result_rx) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let _ = result_tx.send(apply());
        })
        .map_err(|_| "could not schedule the companion opacity update".to_string())?;
    result_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "the companion opacity update timed out".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retina_3024_monitor_has_expected_frames() {
        let monitor = PhysicalMonitor {
            x: 0,
            y: 0,
            width: 3024,
            height: 1964,
            scale_factor: 2.0,
        };

        assert_eq!(
            physical_frame_for_monitor(monitor, PanelMode::Collapsed),
            PhysicalFrame {
                x: 1252,
                y: 0,
                width: 520,
                height: 64,
            }
        );
        assert_eq!(
            physical_frame_for_monitor(monitor, PanelMode::Expanded),
            PhysicalFrame {
                x: 924,
                y: 0,
                width: 1176,
                height: 880,
            }
        );
    }

    #[test]
    fn negative_origin_monitor_remains_centered_at_its_own_top_edge() {
        let monitor = PhysicalMonitor {
            x: -3840,
            y: -2160,
            width: 3840,
            height: 2160,
            scale_factor: 2.0,
        };

        assert_eq!(
            physical_frame_for_monitor(monitor, PanelMode::Collapsed),
            PhysicalFrame {
                x: -2180,
                y: -2160,
                width: 520,
                height: 64,
            }
        );
        assert_eq!(
            physical_frame_for_monitor(monitor, PanelMode::Expanded),
            PhysicalFrame {
                x: -2508,
                y: -2160,
                width: 1176,
                height: 880,
            }
        );
    }

    #[test]
    fn positive_origin_non_retina_monitor_uses_logical_dimensions_directly() {
        let monitor = PhysicalMonitor {
            x: 3024,
            y: 180,
            width: 1920,
            height: 1080,
            scale_factor: 1.0,
        };

        assert_eq!(
            physical_frame_for_monitor(monitor, PanelMode::Collapsed),
            PhysicalFrame {
                x: 3854,
                y: 180,
                width: 260,
                height: 32,
            }
        );
        assert_eq!(
            physical_frame_for_monitor(monitor, PanelMode::Expanded),
            PhysicalFrame {
                x: 3690,
                y: 180,
                width: 588,
                height: 440,
            }
        );
    }

    #[test]
    fn every_dock_position_anchors_to_its_monitor_edge() {
        let monitor = PhysicalMonitor {
            x: 100,
            y: 200,
            width: 1_200,
            height: 900,
            scale_factor: 1.0,
        };
        let expected = [
            (DockPosition::Top, (570, 200, 260, 32)),
            (DockPosition::Left, (100, 520, 32, 260)),
            (DockPosition::Right, (1_268, 520, 32, 260)),
            (DockPosition::Bottom, (570, 1_068, 260, 32)),
            (DockPosition::BottomLeft, (100, 1_068, 32, 32)),
            (DockPosition::BottomRight, (1_268, 1_068, 32, 32)),
        ];

        for (dock, (x, y, width, height)) in expected {
            let frame = physical_frame_for_monitor_at(monitor, PanelMode::Collapsed, dock);
            assert_eq!((frame.x, frame.y), (x, y), "{}", dock.as_str());
            assert_eq!((frame.width, frame.height), (width, height));
        }
    }

    #[test]
    fn dragged_frames_snap_to_the_six_position_vocabulary() {
        let monitor = PhysicalMonitor {
            x: 0,
            y: 0,
            width: 1_200,
            height: 900,
            scale_factor: 1.0,
        };
        let at = |x, y| PhysicalFrame {
            x,
            y,
            width: 150,
            height: 34,
        };
        assert_eq!(
            nearest_dock_for_frame(monitor, at(525, 0)),
            DockPosition::Top
        );
        assert_eq!(
            nearest_dock_for_frame(monitor, at(0, 400)),
            DockPosition::Left
        );
        assert_eq!(
            nearest_dock_for_frame(monitor, at(1_050, 400)),
            DockPosition::Right
        );
        assert_eq!(
            nearest_dock_for_frame(monitor, at(525, 866)),
            DockPosition::Bottom
        );
        assert_eq!(
            nearest_dock_for_frame(monitor, at(0, 866)),
            DockPosition::BottomLeft
        );
        assert_eq!(
            nearest_dock_for_frame(monitor, at(1_050, 866)),
            DockPosition::BottomRight
        );
    }

    #[test]
    fn rotated_and_corner_tabs_remain_collapsed_during_drag() {
        assert_eq!(mode_for_logical_size(260.0, 32.0), PanelMode::Collapsed);
        assert_eq!(mode_for_logical_size(32.0, 260.0), PanelMode::Collapsed);
        assert_eq!(mode_for_logical_size(32.0, 32.0), PanelMode::Collapsed);
        assert_eq!(mode_for_logical_size(588.0, 440.0), PanelMode::Expanded);
    }

    #[test]
    fn odd_pixel_remainder_is_left_biased_and_stable() {
        let monitor = PhysicalMonitor {
            x: 101,
            y: 37,
            width: 2561,
            height: 1440,
            scale_factor: 1.5,
        };

        assert_eq!(
            physical_frame_for_monitor(monitor, PanelMode::Collapsed),
            PhysicalFrame {
                x: 1186,
                y: 37,
                width: 390,
                height: 48,
            }
        );
        assert_eq!(1186 - 101, 2561 - (1186 - 101) - 390 - 1);
    }

    #[test]
    fn panel_states_use_one_native_vocabulary() {
        assert_eq!(PanelMode::from_state("hidden"), Ok(PanelMode::Collapsed));
        assert_eq!(PanelMode::from_state("collapsed"), Ok(PanelMode::Collapsed));
        assert_eq!(PanelMode::from_state("expanded"), Ok(PanelMode::Expanded));
        assert!(PanelMode::from_state("floating").is_err());
    }

    #[test]
    fn dock_positions_serialize_as_native_strings() {
        let positions = [
            DockPosition::Top,
            DockPosition::Left,
            DockPosition::Right,
            DockPosition::Bottom,
            DockPosition::BottomLeft,
            DockPosition::BottomRight,
        ];
        assert_eq!(
            serde_json::to_value(positions).unwrap(),
            serde_json::json!([
                "top",
                "left",
                "right",
                "bottom",
                "bottom-left",
                "bottom-right"
            ])
        );
        assert!(serde_json::from_str::<DockPosition>("\"floating\"").is_err());
    }

    #[test]
    fn invalid_scale_factor_falls_back_to_one_without_invalid_geometry() {
        let base = PhysicalMonitor {
            x: 20,
            y: 30,
            width: 1920,
            height: 1080,
            scale_factor: f64::NAN,
        };
        let invalid_zero = PhysicalMonitor {
            scale_factor: 0.0,
            ..base
        };

        assert_eq!(
            physical_frame_for_monitor(base, PanelMode::Collapsed),
            physical_frame_for_monitor(invalid_zero, PanelMode::Collapsed)
        );
        assert_eq!(
            physical_frame_for_monitor(base, PanelMode::Collapsed),
            PhysicalFrame {
                x: 850,
                y: 30,
                width: 260,
                height: 32,
            }
        );
    }

    #[test]
    fn animation_duration_is_bounded_and_invalid_values_are_immediate() {
        assert_eq!(normalized_animation_duration(None), 0.0);
        assert_eq!(normalized_animation_duration(Some(0.0)), 0.0);
        assert_eq!(normalized_animation_duration(Some(-1.0)), 0.0);
        assert_eq!(normalized_animation_duration(Some(f64::NAN)), 0.0);
        assert_eq!(normalized_animation_duration(Some(f64::INFINITY)), 0.0);
        assert_eq!(normalized_animation_duration(Some(0.18)), 0.18);
        assert_eq!(normalized_animation_duration(Some(10.0)), 2.0);
    }
}
