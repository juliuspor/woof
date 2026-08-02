use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use serde::{Deserialize, Serialize};
use woof_core::{atomic_write_private, ensure_private_dir};
use woof_inline::{ModifierKey, ModifierMonitor};
use woof_llm::CancellationToken;

use crate::{
    companion_panel::DockPosition, inline::InlineCoordinator,
    transcription::TranscriptionCoordinator,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutChord {
    pub meta: bool,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub key: String,
}

impl ShortcutChord {
    pub fn cmd_shift_g() -> Self {
        Self {
            meta: true,
            shift: true,
            alt: false,
            control: false,
            key: "g".into(),
        }
    }

    pub fn accelerator(&self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.meta {
            parts.push("CommandOrControl".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.control {
            parts.push("Control".to_string());
        }
        let key = if self.key.chars().count() == 1 {
            self.key.to_ascii_uppercase()
        } else {
            self.key.clone()
        };
        parts.push(key);
        parts.join("+")
    }
}

impl Default for ShortcutChord {
    fn default() -> Self {
        Self::cmd_shift_g()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub onboarding_done: bool,
    pub contact_name: String,
    pub contact_company: String,
    pub reduce_visual_effects: bool,
    pub caret_sounds_enabled: bool,
    pub voice_dictation_enabled: bool,
    pub capture_paused: bool,
    pub transcription_modifier_key: ModifierKey,
    pub woof_modifier_key: ModifierKey,
    pub woof_modifier_enabled: bool,
    pub secondary_shortcut_enabled: bool,
    pub secondary_shortcut: ShortcutChord,
    pub companion_position: DockPosition,
    pub companion_hover_open: bool,
    #[serde(default)]
    pub companion_hover_open_configured: bool,
    pub collapsed_auto_hide: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            onboarding_done: false,
            contact_name: String::new(),
            contact_company: String::new(),
            reduce_visual_effects: false,
            caret_sounds_enabled: true,
            voice_dictation_enabled: true,
            // Fresh installs stay paused until the user explicitly completes
            // onboarding consent. The daemon also receives --start-paused.
            capture_paused: true,
            transcription_modifier_key: ModifierKey::Fn,
            woof_modifier_key: ModifierKey::RightOption,
            woof_modifier_enabled: true,
            secondary_shortcut_enabled: true,
            secondary_shortcut: ShortcutChord::default(),
            companion_position: DockPosition::Top,
            companion_hover_open: true,
            companion_hover_open_configured: true,
            collapsed_auto_hide: false,
        }
    }
}

impl Preferences {
    fn repair_modifier_collision(&mut self) -> bool {
        if self.woof_modifier_key != self.transcription_modifier_key {
            return false;
        }
        self.transcription_modifier_key = if self.woof_modifier_key == ModifierKey::Fn {
            ModifierKey::RightOption
        } else {
            ModifierKey::Fn
        };
        true
    }

    fn migrate_companion_hover_default(&mut self) -> bool {
        if self.companion_hover_open_configured {
            return false;
        }
        // Older builds persisted `false` as an implicit product default and
        // had no marker that could distinguish it from an explicit opt-out.
        // Adopt the new hover-first behavior once; every subsequent settings
        // write carries the marker and preserves an explicit `false`.
        self.companion_hover_open = true;
        self.companion_hover_open_configured = true;
        true
    }
}

pub struct UiState {
    pub preferences: Mutex<Preferences>,
    pub preferences_path: PathBuf,
    pub chat_cancellation: Mutex<Option<CancellationToken>>,
    pub capture_transition: tokio::sync::Mutex<()>,
    pub transcription: Mutex<TranscriptionCoordinator>,
    pub inline: Mutex<InlineCoordinator>,
    pub modifier_monitor: Mutex<Option<ModifierMonitor>>,
    pub secondary_shortcut_error: Mutex<Option<String>>,
    pub edit_glass_dark: Mutex<bool>,
    pub edit_content_height: Mutex<f64>,
}

impl UiState {
    pub fn load() -> Self {
        let preferences_path = preferences_path();
        let preferences = read_private_preferences(&preferences_path)
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            preferences: Mutex::new(preferences),
            preferences_path,
            chat_cancellation: Mutex::new(None),
            capture_transition: tokio::sync::Mutex::new(()),
            transcription: Mutex::new(TranscriptionCoordinator::default()),
            inline: Mutex::new(InlineCoordinator::default()),
            modifier_monitor: Mutex::new(None),
            secondary_shortcut_error: Mutex::new(None),
            edit_glass_dark: Mutex::new(false),
            edit_content_height: Mutex::new(20.0),
        }
    }

    pub fn read(&self) -> Result<Preferences, String> {
        self.preferences
            .lock()
            .map(|value| value.clone())
            .map_err(|_| "preference state is unavailable".into())
    }

    pub fn update<F>(&self, mutation: F) -> Result<Preferences, String>
    where
        F: FnOnce(&mut Preferences),
    {
        let mut preferences = self
            .preferences
            .lock()
            .map_err(|_| "preference state is unavailable".to_string())?;
        let mut snapshot = preferences.clone();
        mutation(&mut snapshot);
        write_private_json(&self.preferences_path, &snapshot)?;
        *preferences = snapshot.clone();
        Ok(snapshot)
    }

    pub fn secondary_shortcut_error(&self) -> Result<Option<String>, String> {
        self.secondary_shortcut_error
            .lock()
            .map(|error| error.clone())
            .map_err(|_| "shortcut registration state is unavailable".into())
    }

    pub fn set_secondary_shortcut_error(&self, error: Option<String>) -> Result<(), String> {
        *self
            .secondary_shortcut_error
            .lock()
            .map_err(|_| "shortcut registration state is unavailable".to_string())? = error;
        Ok(())
    }
}

fn preferences_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".woof")
        .join("ui.json")
}

fn write_private_json(path: &Path, value: &Preferences) -> Result<(), String> {
    let mut encoded =
        serde_json::to_vec_pretty(value).map_err(|_| "could not encode preferences")?;
    encoded.push(b'\n');
    atomic_write_private(path, &encoded).map_err(|_| "could not save private preferences".into())
}

fn read_private_preferences(path: &Path) -> Result<Option<Preferences>, String> {
    const MAX_PREFERENCES_BYTES: u64 = 64 * 1024;

    let parent = path
        .parent()
        .ok_or_else(|| "preference path has no parent".to_string())?;
    ensure_private_dir(parent).map_err(|_| "could not secure the woof config directory")?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("refusing to read a symlinked preference file".into())
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err("preference path is not a regular file".into())
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("could not inspect private preferences".into()),
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .map_err(|_| "could not open private preferences".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "could not inspect private preferences".to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_PREFERENCES_BYTES {
        return Err("private preferences are not a bounded regular file".into());
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "could not repair private preference permissions".to_string())?;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_PREFERENCES_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "could not read private preferences".to_string())?;
    if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err("private preferences are too large".into());
    }
    let mut preferences: Preferences = serde_json::from_slice(&bytes)
        .map_err(|_| "private preferences are invalid".to_string())?;
    let repaired_modifiers = preferences.repair_modifier_collision();
    let migrated_hover_default = preferences.migrate_companion_hover_default();
    if repaired_modifiers || migrated_hover_default {
        write_private_json(path, &preferences)?;
    }
    Ok(Some(preferences))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_native_structured_shortcut_shape() {
        let preferences: Preferences = serde_json::from_value(serde_json::json!({
            "secondary_shortcut": {
                "meta": true,
                "shift": true,
                "alt": false,
                "control": false,
                "key": "w"
            },
            "woof_modifier_key": "left_option",
            "transcription_modifier_key": "right_option"
        }))
        .unwrap();
        assert_eq!(
            preferences.secondary_shortcut,
            ShortcutChord {
                meta: true,
                shift: true,
                alt: false,
                control: false,
                key: "w".into(),
            }
        );
        assert_eq!(
            preferences.secondary_shortcut.accelerator(),
            "CommandOrControl+Shift+W"
        );
        assert_eq!(preferences.woof_modifier_key, ModifierKey::LeftOption);
        assert_eq!(
            preferences.transcription_modifier_key,
            ModifierKey::RightOption
        );
    }

    #[test]
    fn serializes_the_native_preferences_contract() {
        let encoded = serde_json::to_value(Preferences::default()).unwrap();
        assert_eq!(encoded["capture_paused"], true);
        assert_eq!(
            encoded["secondary_shortcut"],
            serde_json::json!({
                "meta": true,
                "shift": true,
                "alt": false,
                "control": false,
                "key": "g"
            })
        );
        assert_eq!(encoded["woof_modifier_key"], "right_option");
        assert_eq!(encoded["transcription_modifier_key"], "fn");
        assert_eq!(encoded["companion_position"], "top");
        assert_eq!(encoded["companion_hover_open"], true);
        assert_eq!(encoded["companion_hover_open_configured"], true);
    }

    #[test]
    fn private_preferences_migrate_the_legacy_hover_default_only_once() {
        let directory =
            std::env::temp_dir().join(format!("woof-ui-hover-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ui.json");
        let mut legacy = serde_json::to_value(Preferences::default()).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("companion_hover_open_configured");
        legacy["companion_hover_open"] = serde_json::json!(false);
        fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let migrated = read_private_preferences(&path).unwrap().unwrap();
        assert!(migrated.companion_hover_open);
        assert!(migrated.companion_hover_open_configured);

        let mut explicit_opt_out = migrated;
        explicit_opt_out.companion_hover_open = false;
        write_private_json(&path, &explicit_opt_out).unwrap();
        let reloaded = read_private_preferences(&path).unwrap().unwrap();
        assert!(!reloaded.companion_hover_open);
        assert!(reloaded.companion_hover_open_configured);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn private_preferences_repair_colliding_modifier_keys_before_use() {
        let directory =
            std::env::temp_dir().join(format!("woof-ui-modifiers-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ui.json");
        let mut preferences = Preferences::default();
        preferences.transcription_modifier_key = preferences.woof_modifier_key;
        write_private_json(&path, &preferences).unwrap();

        let repaired = read_private_preferences(&path).unwrap().unwrap();
        assert_ne!(
            repaired.woof_modifier_key,
            repaired.transcription_modifier_key
        );
        let persisted: Preferences = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_ne!(
            persisted.woof_modifier_key,
            persisted.transcription_modifier_key
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_preferences_repair_mode_and_reject_symlinks() {
        use std::os::unix::fs::symlink;

        let directory =
            std::env::temp_dir().join(format!("woof-ui-state-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ui.json");
        fs::write(&path, serde_json::to_vec(&Preferences::default()).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        assert!(read_private_preferences(&path).unwrap().is_some());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let link = directory.join("linked.json");
        symlink(&path, &link).unwrap();
        assert!(read_private_preferences(&link).is_err());
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn native_preferences_reject_unknown_fields() {
        assert!(serde_json::from_value::<Preferences>(serde_json::json!({
            "unknown": true
        }))
        .is_err());
        assert!(serde_json::from_value::<ShortcutChord>(serde_json::json!({
            "meta": true,
            "shift": true,
            "alt": false,
            "control": false,
            "key": "g",
            "unknown": true
        }))
        .is_err());
    }

    #[test]
    fn failed_preference_writes_do_not_change_runtime_state() {
        let directory =
            std::env::temp_dir().join(format!("woof-ui-update-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let blocking_file = directory.join("not-a-directory");
        fs::write(&blocking_file, b"block").unwrap();
        let state = UiState {
            preferences: Mutex::new(Preferences::default()),
            preferences_path: blocking_file.join("ui.json"),
            chat_cancellation: Mutex::new(None),
            capture_transition: tokio::sync::Mutex::new(()),
            transcription: Mutex::new(TranscriptionCoordinator::default()),
            inline: Mutex::new(InlineCoordinator::default()),
            modifier_monitor: Mutex::new(None),
            secondary_shortcut_error: Mutex::new(None),
            edit_glass_dark: Mutex::new(false),
            edit_content_height: Mutex::new(20.0),
        };

        assert!(state
            .update(|preferences| preferences.capture_paused = false)
            .is_err());
        assert!(state.read().unwrap().capture_paused);
        fs::remove_dir_all(directory).unwrap();
    }
}
