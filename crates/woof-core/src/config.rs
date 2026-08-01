use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use url::Host;

use crate::{
    atomic_write_private, ensure_private_dir, read_private_file_bounded, PrivateFileError,
    WoofPaths, DEFAULT_BIND,
};

const MAX_CONFIG_BYTES: usize = 256 * 1024;

pub const MAX_CAPTURE_BLACKLIST_ENTRIES: usize = 100;
pub const MAX_CAPTURE_BLACKLIST_PATTERN_BYTES: usize = 2_048;
pub const MIN_RETENTION_DAYS: u16 = 1;
pub const MAX_RETENTION_DAYS: u16 = 3_650;

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum CaptureBlacklistError {
    #[error("the capture blacklist supports at most 100 rules")]
    TooManyEntries,
    #[error("capture blacklist rule kind is unsupported")]
    UnsupportedKind,
    #[error("every capture blacklist rule needs a pattern")]
    EmptyPattern,
    #[error("capture blacklist rule pattern is too long")]
    PatternTooLong,
    #[error("capture blacklist regular expression is invalid")]
    InvalidRegex,
    #[error("capture blacklist browser host is invalid")]
    InvalidBrowserHost,
}

impl CaptureBlacklistError {
    pub const fn user_message(self) -> &'static str {
        match self {
            Self::TooManyEntries => "the capture blacklist supports at most 100 rules",
            Self::UnsupportedKind => "capture blacklist rule kind is unsupported",
            Self::EmptyPattern => "every capture blacklist rule needs a pattern",
            Self::PatternTooLong => "capture blacklist rule pattern is too long",
            Self::InvalidRegex => "capture blacklist regular expression is invalid",
            Self::InvalidBrowserHost => "capture blacklist browser host is invalid",
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    PrivateFile(#[from] PrivateFileError),
    #[error("failed to read configuration at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid configuration at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("woof must bind its daemon to port 3334")]
    InvalidPort,
    #[error("woof runtime paths must remain inside its private application directories")]
    InvalidRuntimePath,
    #[error(transparent)]
    CaptureBlacklist(#[from] CaptureBlacklistError),
    #[error("data retention must be between 1 and 3650 days")]
    InvalidRetention,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DataRetentionPolicy {
    KeepForever,
    Days { days: u16 },
}

impl<'de> Deserialize<'de> for DataRetentionPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
        enum WirePolicy {
            KeepForever {},
            Days { days: u16 },
        }

        Ok(match WirePolicy::deserialize(deserializer)? {
            WirePolicy::KeepForever {} => Self::KeepForever,
            WirePolicy::Days { days } => Self::Days { days },
        })
    }
}

impl DataRetentionPolicy {
    pub fn validate(self) -> Result<(), ConfigError> {
        match self {
            Self::KeepForever => Ok(()),
            Self::Days { days } if (MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) => {
                Ok(())
            }
            Self::Days { .. } => Err(ConfigError::InvalidRetention),
        }
    }

    pub fn cutoff(self, now: i64) -> Option<i64> {
        match self {
            Self::KeepForever => None,
            Self::Days { days } => Some(now.saturating_sub(i64::from(days) * 86_400)),
        }
    }
}

impl Default for DataRetentionPolicy {
    fn default() -> Self {
        Self::KeepForever
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureBlacklistEntry {
    pub kind: String,
    pub pattern: String,
}

pub fn normalize_capture_blacklist(
    entries: Vec<CaptureBlacklistEntry>,
) -> Result<Vec<CaptureBlacklistEntry>, CaptureBlacklistError> {
    if entries.len() > MAX_CAPTURE_BLACKLIST_ENTRIES {
        return Err(CaptureBlacklistError::TooManyEntries);
    }
    entries
        .into_iter()
        .map(|mut entry| {
            entry.kind = entry.kind.trim().to_ascii_lowercase();
            entry.pattern = entry.pattern.trim().to_owned();
            validate_capture_blacklist_entry(&entry)?;
            Ok(entry)
        })
        .collect()
}

pub fn validate_capture_blacklist(
    entries: &[CaptureBlacklistEntry],
) -> Result<(), CaptureBlacklistError> {
    if entries.len() > MAX_CAPTURE_BLACKLIST_ENTRIES {
        return Err(CaptureBlacklistError::TooManyEntries);
    }
    entries
        .iter()
        .try_for_each(validate_capture_blacklist_entry)
}

fn validate_capture_blacklist_entry(
    entry: &CaptureBlacklistEntry,
) -> Result<(), CaptureBlacklistError> {
    if !matches!(
        entry.kind.as_str(),
        "bundle_id" | "bundle_prefix" | "app_name" | "window_title" | "browser_host" | "regex"
    ) {
        return Err(CaptureBlacklistError::UnsupportedKind);
    }
    if entry.pattern.trim().is_empty() {
        return Err(CaptureBlacklistError::EmptyPattern);
    }
    if entry.pattern.len() > MAX_CAPTURE_BLACKLIST_PATTERN_BYTES {
        return Err(CaptureBlacklistError::PatternTooLong);
    }
    if entry.kind == "regex" && regex::Regex::new(&entry.pattern).is_err() {
        return Err(CaptureBlacklistError::InvalidRegex);
    }
    if entry.kind == "browser_host" && !valid_browser_host_pattern(&entry.pattern) {
        return Err(CaptureBlacklistError::InvalidBrowserHost);
    }
    Ok(())
}

fn valid_browser_host_pattern(value: &str) -> bool {
    let value = value.trim_end_matches('.');
    if value.is_empty() {
        return false;
    }
    if value.contains(':') && !(value.starts_with('[') && value.ends_with(']')) {
        Host::parse(&format!("[{value}]")).is_ok()
    } else {
        Host::parse(value).is_ok()
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WoofConfig {
    pub db_path: PathBuf,
    pub identity_path: PathBuf,
    pub log_dir: PathBuf,
    pub api_port: u16,
    pub capture_interval_ms: u64,
    pub coalesce_window_secs: u64,
    pub working_memory_capacity: usize,
    pub capture_blacklist: Vec<CaptureBlacklistEntry>,
    pub nudges_enabled: bool,
    pub data_retention: DataRetentionPolicy,
}

impl WoofConfig {
    pub fn for_paths(paths: &WoofPaths) -> Self {
        Self {
            db_path: paths.db_path.clone(),
            identity_path: paths.identity_path.clone(),
            log_dir: paths.log_dir.clone(),
            ..Self::default()
        }
    }

    pub fn bind_address(&self) -> String {
        format!("127.0.0.1:{}", self.api_port)
    }

    pub fn load_or_create(paths: &WoofPaths) -> Result<Self, ConfigError> {
        ensure_private_dir(&paths.config_dir)?;
        ensure_private_dir(&paths.data_dir)?;
        ensure_private_dir(&paths.log_dir)?;
        let config = match fs::symlink_metadata(&paths.config_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(PrivateFileError::Symlink(paths.config_path.clone()).into())
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(PrivateFileError::NotRegularFile(paths.config_path.clone()).into())
            }
            Ok(_) => {
                let bytes = read_private_file_bounded(&paths.config_path, MAX_CONFIG_BYTES)?;
                serde_json::from_slice::<Self>(&bytes).map_err(|source| ConfigError::Json {
                    path: paths.config_path.clone(),
                    source,
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::for_paths(paths),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: paths.config_path.clone(),
                    source,
                })
            }
        };
        config.validate()?;
        config.validate_runtime_paths(paths)?;
        config.save(&paths.config_path)?;
        Ok(config)
    }

    /// Loads configuration and replaces only malformed or invalid logical
    /// contents with the privacy-safe defaults. Unsafe path types and I/O
    /// failures remain terminal.
    pub fn load_or_replace_invalid(paths: &WoofPaths) -> Result<Self, ConfigError> {
        match Self::load_or_create(paths) {
            Ok(config) => Ok(config),
            Err(
                ConfigError::Json { .. }
                | ConfigError::InvalidPort
                | ConfigError::InvalidRuntimePath
                | ConfigError::CaptureBlacklist(_)
                | ConfigError::InvalidRetention,
            ) => {
                let config = Self::for_paths(paths);
                config.save(&paths.config_path)?;
                Ok(config)
            }
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|source| ConfigError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        bytes.push(b'\n');
        atomic_write_private(path, &bytes)?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.api_port != 3334 {
            return Err(ConfigError::InvalidPort);
        }
        validate_capture_blacklist(&self.capture_blacklist)?;
        self.data_retention.validate()?;
        Ok(())
    }

    fn validate_runtime_paths(&self, paths: &WoofPaths) -> Result<(), ConfigError> {
        if self.db_path != paths.db_path
            || self.identity_path != paths.identity_path
            || self.log_dir != paths.log_dir
        {
            return Err(ConfigError::InvalidRuntimePath);
        }
        Ok(())
    }
}

impl Default for WoofConfig {
    fn default() -> Self {
        let paths = WoofPaths::discover().unwrap_or_else(|| {
            WoofPaths::from_roots(PathBuf::from(".woof"), PathBuf::from("woof-data"))
        });
        Self {
            db_path: paths.db_path,
            identity_path: paths.identity_path,
            log_dir: paths.log_dir,
            api_port: DEFAULT_BIND
                .rsplit_once(':')
                .and_then(|(_, port)| port.parse().ok())
                .unwrap_or(3334),
            capture_interval_ms: 1_000,
            coalesce_window_secs: 30,
            working_memory_capacity: 200,
            capture_blacklist: Vec::new(),
            nudges_enabled: false,
            data_retention: DataRetentionPolicy::KeepForever,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_paths() -> WoofPaths {
        WoofPaths::from_roots(
            PathBuf::from("/tmp/woof-test-config"),
            PathBuf::from("/tmp/woof-test-data"),
        )
    }

    #[test]
    fn runtime_paths_are_pinned_to_woof_directories() {
        let paths = fixture_paths();
        let config = WoofConfig::for_paths(&paths);
        assert!(config.validate_runtime_paths(&paths).is_ok());

        let mut redirected = config;
        redirected.db_path = PathBuf::from("/tmp/other.db");
        assert!(matches!(
            redirected.validate_runtime_paths(&paths),
            Err(ConfigError::InvalidRuntimePath)
        ));
    }

    #[test]
    fn capture_blacklist_is_normalized_and_validated() {
        let entries = normalize_capture_blacklist(vec![CaptureBlacklistEntry {
            kind: " REGEX ".into(),
            pattern: "  private-[0-9]+  ".into(),
        }])
        .unwrap();
        assert_eq!(
            entries,
            vec![CaptureBlacklistEntry {
                kind: "regex".into(),
                pattern: "private-[0-9]+".into(),
            }]
        );
        assert_eq!(
            normalize_capture_blacklist(vec![CaptureBlacklistEntry {
                kind: "regex".into(),
                pattern: "(".into(),
            }]),
            Err(CaptureBlacklistError::InvalidRegex)
        );
        assert_eq!(
            normalize_capture_blacklist(vec![CaptureBlacklistEntry {
                kind: "unknown".into(),
                pattern: "secret".into(),
            }]),
            Err(CaptureBlacklistError::UnsupportedKind)
        );

        for pattern in [
            "example.com",
            "example.com.",
            "127.0.0.1",
            "[2001:db8::1]",
            "2001:0db8:0:0:0:0:0:1",
        ] {
            assert!(normalize_capture_blacklist(vec![CaptureBlacklistEntry {
                kind: "browser_host".into(),
                pattern: pattern.into(),
            }])
            .is_ok());
        }
        for pattern in [
            "https://example.com",
            "example.com/private",
            "user@example.com",
            "example.com:443",
            "[2001:db8::1]:443",
        ] {
            assert_eq!(
                normalize_capture_blacklist(vec![CaptureBlacklistEntry {
                    kind: "browser_host".into(),
                    pattern: pattern.into(),
                }]),
                Err(CaptureBlacklistError::InvalidBrowserHost),
                "pattern {pattern}"
            );
        }

        let config = WoofConfig {
            capture_blacklist: vec![CaptureBlacklistEntry {
                kind: "browser_host".into(),
                pattern: "https://example.com".into(),
            }],
            ..WoofConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::CaptureBlacklist(
                CaptureBlacklistError::InvalidBrowserHost
            ))
        ));
    }

    #[test]
    fn retention_is_bounded_and_produces_a_cutoff() {
        assert_eq!(DataRetentionPolicy::KeepForever.cutoff(100), None);
        assert_eq!(
            DataRetentionPolicy::Days { days: 2 }.cutoff(200_000),
            Some(27_200)
        );
        assert!(DataRetentionPolicy::Days { days: 0 }.validate().is_err());
        assert!(DataRetentionPolicy::Days {
            days: MAX_RETENTION_DAYS + 1
        }
        .validate()
        .is_err());
        assert!(
            serde_json::from_value::<DataRetentionPolicy>(serde_json::json!({
                "mode": "keep_forever",
                "days": 1
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<DataRetentionPolicy>(serde_json::json!({
                "mode": "days",
                "days": 30,
                "unexpected": true
            }))
            .is_err()
        );
    }
}
