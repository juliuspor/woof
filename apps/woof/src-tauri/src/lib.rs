mod caret_sound;
mod chat_tools;
mod commands;
mod companion_panel;
mod inline;
mod login_item;
mod notifications;
mod state;
mod supervisor;
mod transcription;

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager, RunEvent, WindowEvent,
};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};

use state::{ShortcutChord, UiState};
use supervisor::DaemonSupervisor;

const PAUSE_CAPTURE_LABEL: &str = "Pause capture";
const RESUME_CAPTURE_LABEL: &str = "Resume capture";

struct CaptureTrayMenuItem(MenuItem<tauri::Wry>);

fn capture_tray_label(paused: bool) -> &'static str {
    if paused {
        RESUME_CAPTURE_LABEL
    } else {
        PAUSE_CAPTURE_LABEL
    }
}

pub(crate) fn sync_capture_tray_label(app: &tauri::AppHandle, paused: bool) {
    if let Some(item) = app.try_state::<CaptureTrayMenuItem>() {
        let _ = item.0.set_text(capture_tray_label(paused));
    }
}

fn install_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open_memory_hub = MenuItem::with_id(
        app,
        "open-memory-hub",
        "Open memory hub",
        true,
        None::<&str>,
    )?;
    let open_chat = MenuItem::with_id(app, "open-chat", "Ask woof", true, None::<&str>)?;
    let initially_paused = app
        .state::<UiState>()
        .read()
        .map(|preferences| preferences.capture_paused)
        .unwrap_or(true);
    let pause = MenuItem::with_id(
        app,
        "pause-capture",
        capture_tray_label(initially_paused),
        true,
        None::<&str>,
    )?;
    let _ = app.manage(CaptureTrayMenuItem(pause.clone()));
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit woof", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open_memory_hub,
            &open_chat,
            &pause,
            &settings,
            &separator,
            &quit,
        ],
    )?;
    let icon = Image::from_bytes(include_bytes!("../icons/woof-menubar-Template.png"))?;

    TrayIconBuilder::with_id("woof")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("woof")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open-memory-hub" => {
                if let Some(window) = app.get_webview_window("memory-hub") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "open-chat" => {
                let _ = commands::open_companion_focused(app);
            }
            "pause-capture" => {
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    let paused = commands::capture_is_paused(handle.clone(), handle.state())
                        .await
                        .unwrap_or(false);
                    let _ = if paused {
                        commands::capture_resume(handle.clone(), handle.state()).await
                    } else {
                        commands::capture_pause(handle.clone(), handle.state()).await
                    };
                });
            }
            "settings" => {
                if commands::open_companion_focused(app).is_ok() {
                    if let Some(window) = app.get_webview_window(companion_panel::WINDOW_LABEL) {
                        let _ = window.emit("woof:open-settings", serde_json::json!({}));
                    }
                }
            }
            "quit" => {
                app.state::<DaemonSupervisor>().shutdown();
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn configure_windows(app: &tauri::AppHandle) {
    for label in [
        companion_panel::WINDOW_LABEL,
        "onboarding",
        "permission",
        "caret-overlay",
        "edit-mode",
        "health",
    ] {
        if let Some(window) = app.get_webview_window(label) {
            let corner_radius = if label == companion_panel::WINDOW_LABEL {
                16.0
            } else {
                18.0
            };
            let _ = apply_vibrancy(
                &window,
                NSVisualEffectMaterial::HudWindow,
                Some(NSVisualEffectState::Active),
                Some(corner_radius),
            );
        }
    }

    if let Some(caret) = app.get_webview_window("caret-overlay") {
        let _ = caret.set_ignore_cursor_events(false);
    }

    if let Some(memory_hub) = app.get_webview_window("memory-hub") {
        // Tao creates undecorated macOS windows without the native closable
        // style bit. Reapply it after construction so the standard Close
        // Window menu item and Command-W deliver CloseRequested; the handler
        // below then hides this persistent window instead of destroying it.
        let _ = memory_hub.set_closable(true);
    }

    if let Some(companion) = app.get_webview_window(companion_panel::WINDOW_LABEL) {
        let dock = app
            .state::<UiState>()
            .read()
            .map(|preferences| preferences.companion_position)
            .unwrap_or_default();
        let _ = companion_panel::configure_at(&companion, dock);
        let _ = companion_panel::install_hover_tracking(&companion);
    }
}

fn publish_permission_state(app: &tauri::AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let accessibility = commands::accessibility_trusted(handle.clone()).await;
        let input_monitoring = commands::input_monitoring_trusted(handle.clone());
        let microphone = commands::microphone_status(None, None)
            .await
            .unwrap_or("unavailable");
        let _ = handle.emit(
            "woof:permissions-changed",
            serde_json::json!({
                "accessibility": accessibility,
                "inputMonitoring": input_monitoring,
                "microphone": microphone,
            }),
        );
    });
}

pub(crate) fn install_shortcuts(app: &tauri::AppHandle) -> Result<(), String> {
    let preferences = app.state::<UiState>().read().unwrap_or_default();
    if preferences.secondary_shortcut_enabled {
        install_shortcut_chord(app, &preferences.secondary_shortcut)
    } else {
        unregister_shortcuts(app)
    }
}

pub(crate) fn unregister_shortcuts(app: &tauri::AppHandle) -> Result<(), String> {
    app.global_shortcut()
        .unregister_all()
        .map_err(|_| "could not unregister the secondary shortcut".to_string())
}

pub(crate) fn install_shortcut_chord(
    app: &tauri::AppHandle,
    chord: &ShortcutChord,
) -> Result<(), String> {
    unregister_shortcuts(app)?;
    let accelerator = chord.accelerator();
    app.global_shortcut()
        .on_shortcut(accelerator.as_str(), |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                commands::handle_secondary_shortcut(app.clone());
            }
        })
        .map_err(|_| {
            "the secondary shortcut is unavailable or conflicts with another app".to_string()
        })
}

fn dispatch_woof_deep_links(app: &tauri::AppHandle, urls: Vec<url::Url>) {
    for url in urls.into_iter().take(4) {
        let _ = commands::handle_woof_deep_link(app, &url);
    }
}

fn install_deep_link_handler(app: &tauri::AppHandle) {
    let event_app = app.clone();
    app.deep_link().on_open_url(move |event| {
        dispatch_woof_deep_links(&event_app, event.urls());
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .manage(UiState::load())
        .manage(DaemonSupervisor::new().expect("failed to initialize woof daemon supervision"))
        .invoke_handler(tauri::generate_handler![
            commands::skip_onboarding_cmd,
            commands::finish_onboarding,
            commands::memory_hub_open_route,
            commands::open_onboarding_window_cmd,
            commands::save_contact_info,
            commands::load_contact_info,
            commands::accessibility_status,
            commands::accessibility_trusted,
            commands::request_accessibility,
            commands::open_accessibility_settings,
            commands::microphone_status,
            commands::input_monitoring_trusted,
            commands::request_input_monitoring,
            commands::open_input_monitoring_settings,
            commands::companion_chat_get_position,
            commands::companion_chat_set_position,
            commands::companion_chat_set_state,
            commands::companion_chat_open_focused,
            commands::companion_chat_rollup,
            commands::companion_chat_pointer_ready,
            commands::companion_chat_get_hover_open,
            commands::companion_chat_set_hover_open,
            commands::companion_chat_get_collapsed_auto_hide,
            commands::companion_chat_set_collapsed_auto_hide,
            commands::companion_chat_drag_start,
            commands::companion_chat_drag_frame,
            commands::companion_chat_drag_end,
            commands::companion_chat_set_nudge_card_active,
            commands::companion_chat_set_notification_active,
            commands::companion_open_nudge,
            commands::companion_dismiss_nudge,
            commands::notification_open_settings,
            commands::get_nudges_enabled,
            commands::set_nudges_enabled,
            commands::scheduled_reminder_list,
            commands::scheduled_reminder_create,
            commands::scheduled_reminder_delete,
            commands::chat_send,
            commands::chat_cancel,
            commands::generate_chat_suggestions,
            commands::caret_overlay_ready,
            commands::caret_overlay_cancel,
            commands::edit_mode_ready,
            commands::edit_mode_close,
            commands::edit_mode_set_content_height,
            commands::edit_mode_set_glass_appearance,
            commands::edit_mode_submit,
            commands::transcription_start,
            commands::transcription_finalize,
            commands::transcription_cancel,
            commands::memory_recent_activity,
            commands::memory_working_memory,
            commands::memory_wiki_list,
            commands::memory_wiki_page,
            commands::memory_wiki_search,
            commands::memory_followups,
            commands::memory_followup_set_status,
            commands::memory_work_patterns,
            commands::memory_work_pattern_set_status,
            commands::capture_status,
            commands::get_capture_blacklist,
            commands::set_capture_blacklist,
            commands::memory_delete_all,
            commands::get_data_retention,
            commands::set_data_retention,
            commands::memory_time_report,
            commands::memory_time_rules,
            commands::memory_identity_save,
            commands::capture_is_paused,
            commands::capture_pause,
            commands::capture_resume,
            commands::get_reduce_visual_effects,
            commands::set_reduce_visual_effects,
            commands::get_caret_sounds_enabled,
            commands::set_caret_sounds_enabled,
            commands::get_voice_dictation_enabled,
            commands::set_voice_dictation_enabled,
            commands::get_transcription_modifier_key,
            commands::set_transcription_modifier_key,
            commands::record_modifier_key,
            commands::get_default_woof_modifier_key,
            commands::get_woof_modifier_key,
            commands::set_woof_modifier_key,
            commands::set_modifier_keys,
            commands::get_woof_modifier_enabled,
            commands::set_woof_modifier_enabled,
            commands::get_secondary_shortcut,
            commands::set_secondary_shortcut,
            commands::get_secondary_shortcut_enabled,
            commands::get_secondary_shortcut_error,
            commands::set_secondary_shortcut_enabled,
            commands::record_secondary_shortcut,
            commands::get_api_key_status,
            commands::set_openai_api_key,
            commands::clear_openai_api_key,
            commands::mcp_client_configuration,
            commands::get_login_item_enabled,
            commands::set_login_item_enabled,
            commands::daemon_health,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            install_deep_link_handler(app.handle());
            install_tray(app.handle())?;
            configure_windows(app.handle());
            if let Err(error) = install_shortcuts(app.handle()) {
                let state = app.state::<UiState>();
                let _ = state.update(|preferences| {
                    preferences.secondary_shortcut_enabled = false;
                });
                let _ = state.set_secondary_shortcut_error(Some(error.clone()));
                let _ = app.emit("woof:shortcut-error", error);
            }
            if inline::ensure_modifier_monitor(app.handle()).is_err() {
                let _ = app.emit(
                    "woof:inline-refused",
                    serde_json::json!({"reason": "permission-denied"}),
                );
            }
            app.state::<DaemonSupervisor>().start(app.handle().clone());
            notifications::start(app.handle().clone());
            publish_permission_state(app.handle());

            let onboarding_done = app.state::<UiState>().read()?.onboarding_done;
            let initial = if onboarding_done {
                companion_panel::WINDOW_LABEL
            } else {
                "onboarding"
            };
            if let Some(window) = app.get_webview_window(initial) {
                window.show()?;
                if initial == "onboarding" {
                    window.set_focus()?;
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == companion_panel::WINDOW_LABEL
                && matches!(event, WindowEvent::ScaleFactorChanged { .. })
            {
                if let Some(companion) = window
                    .app_handle()
                    .get_webview_window(companion_panel::WINDOW_LABEL)
                {
                    let dock = window
                        .app_handle()
                        .state::<UiState>()
                        .read()
                        .map(|preferences| preferences.companion_position)
                        .unwrap_or_default();
                    let _ = companion_panel::redock_current_mode_at(&companion, dock);
                }
            }
            if matches!(event, WindowEvent::CloseRequested { .. }) && window.label() != "onboarding"
            {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        });

    let app = builder
        .build(tauri::generate_context!())
        .expect("failed to build woof");
    app.run(|handle, event| {
        if matches!(event, RunEvent::Ready) {
            if let Ok(Some(urls)) = handle.deep_link().get_current() {
                let ready_app = handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    dispatch_woof_deep_links(&ready_app, urls);
                });
            }
        }
        if matches!(event, RunEvent::Resumed | RunEvent::Reopen { .. }) {
            publish_permission_state(handle);
        }
        if matches!(event, RunEvent::Exit) {
            inline::stop_modifier_monitor(handle);
            handle.state::<DaemonSupervisor>().shutdown();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_tray_label_describes_the_available_action() {
        assert_eq!(capture_tray_label(true), "Resume capture");
        assert_eq!(capture_tray_label(false), "Pause capture");
    }
}
