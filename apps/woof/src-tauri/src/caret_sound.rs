//! Quiet, local-only feedback for an explicitly enabled inline-help cue.

#[cfg(target_os = "macos")]
pub fn play_open_cue() {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSString;

    thread_local! {
        static ACTIVE_CUE: RefCell<Option<Retained<NSSound>>> = const { RefCell::new(None) };
    }

    let Some(sound) = NSSound::soundNamed(&NSString::from_str("Tink")) else {
        return;
    };
    sound.setVolume(0.16);
    sound.setCurrentTime(0.0);
    if sound.play() {
        ACTIVE_CUE.with(|active| *active.borrow_mut() = Some(sound));
    }
}

#[cfg(not(target_os = "macos"))]
pub fn play_open_cue() {}
