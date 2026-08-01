#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
fn restrict_process_file_creation() {
    unsafe extern "C" {
        fn umask(mask: u16) -> u16;
    }
    // SAFETY: `umask` changes only this process's file-creation mask. Darwin's
    // `mode_t` is a `u16`, and 0o077 removes group/other permissions.
    unsafe {
        umask(0o077);
    }
}

fn main() {
    #[cfg(target_os = "macos")]
    restrict_process_file_creation();
    woof_lib::run();
}
