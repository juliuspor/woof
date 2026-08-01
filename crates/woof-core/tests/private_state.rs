use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use woof_core::{private_file_mode, ApiToken, WoofConfig, WoofPaths, TOKEN_BYTES};

fn temporary_directory(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("woof-{label}-{}-{unique}", std::process::id()));
    fs::create_dir_all(&path).expect("create temporary directory");
    path
}

fn remove_directory(path: &Path) {
    let _ = fs::remove_dir_all(path);
}

#[test]
fn token_creation_is_private_and_stable() {
    let directory = temporary_directory("token");
    let path = directory.join("api-token");
    let first = ApiToken::load_or_create(&path).expect("create token");
    let second = ApiToken::load_or_create(&path).expect("load token");

    assert_eq!(fs::read(&path).expect("read").len(), TOKEN_BYTES);
    assert!(first.matches_bearer(second.expose()));
    #[cfg(unix)]
    assert_eq!(private_file_mode(&path).expect("mode"), Some(0o600));
    remove_directory(&directory);
}

#[test]
fn malformed_tokens_are_rejected_instead_of_silently_rotated() {
    let directory = temporary_directory("bad-token");
    let path = directory.join("api-token");
    fs::write(&path, b"not-a-token").expect("fixture");
    assert!(ApiToken::load_or_create(&path).is_err());
    assert_eq!(fs::read(&path).expect("unchanged"), b"not-a-token");
    remove_directory(&directory);
}

#[test]
fn explicit_startup_replacement_repairs_only_malformed_token_contents() {
    let directory = temporary_directory("replace-token");
    let path = directory.join("api-token");
    fs::write(&path, b"not-a-token").expect("fixture");

    let token = ApiToken::load_or_replace_invalid(&path).expect("replace malformed token");

    assert_eq!(fs::read(&path).expect("read").len(), TOKEN_BYTES);
    assert!(token.matches_bearer(&fs::read(&path).expect("token bytes")));
    #[cfg(unix)]
    assert_eq!(private_file_mode(&path).expect("mode"), Some(0o600));
    remove_directory(&directory);
}

#[test]
fn configuration_is_private_and_keeps_all_fields() {
    let directory = temporary_directory("config");
    let paths = WoofPaths::from_roots(directory.join(".woof"), directory.join("data"));
    let config = WoofConfig::load_or_create(&paths).expect("configuration");
    assert_eq!(config.api_port, 3334);
    assert!(!config.nudges_enabled);
    let object = serde_json::from_slice::<serde_json::Value>(
        &fs::read(&paths.config_path).expect("read configuration"),
    )
    .expect("JSON");
    assert_eq!(object.as_object().expect("object").len(), 10);
    #[cfg(unix)]
    assert_eq!(
        private_file_mode(&paths.config_path).expect("mode"),
        Some(0o600)
    );
    remove_directory(&directory);
}

#[test]
fn startup_replacement_resets_malformed_configuration_to_safe_paths() {
    let directory = temporary_directory("replace-config");
    let paths = WoofPaths::from_roots(directory.join(".woof"), directory.join("data"));
    fs::create_dir_all(&paths.config_dir).expect("config directory");
    fs::write(&paths.config_path, b"{ malformed").expect("fixture");

    let config = WoofConfig::load_or_replace_invalid(&paths).expect("replace configuration");

    assert_eq!(config.api_port, 3334);
    assert_eq!(config.db_path, paths.db_path);
    assert_eq!(config.identity_path, paths.identity_path);
    serde_json::from_slice::<serde_json::Value>(
        &fs::read(&paths.config_path).expect("read configuration"),
    )
    .expect("valid JSON");
    #[cfg(unix)]
    assert_eq!(
        private_file_mode(&paths.config_path).expect("mode"),
        Some(0o600)
    );
    remove_directory(&directory);
}
