mod accessibility;
mod clipboard;
mod input;
mod monitor;

pub use accessibility::MacOsFocusedTarget;
pub use clipboard::MacOsClipboard;
pub use input::MacOsInputInjector;
pub use monitor::{
    input_monitoring_trusted, record_modifier_key, record_shortcut_chord, request_input_monitoring,
    ModifierMonitor, ModifierMonitorHandle, RecordedShortcutChord,
};
