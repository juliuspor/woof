//! Shared filesystem, configuration, and authentication primitives for woof.

mod api_token;
mod config;
mod health_proof;
mod paths;
mod secure_file;

pub use api_token::{ApiToken, ApiTokenError, TOKEN_BYTES};
pub use config::{
    normalize_capture_blacklist, validate_capture_blacklist, CaptureBlacklistEntry,
    CaptureBlacklistError, ConfigError, DataRetentionPolicy, WoofConfig,
    MAX_CAPTURE_BLACKLIST_ENTRIES, MAX_CAPTURE_BLACKLIST_PATTERN_BYTES, MAX_RETENTION_DAYS,
    MIN_RETENTION_DAYS,
};
pub use health_proof::{
    generate_health_challenge, health_proof, verify_health_proof, HEALTH_CHALLENGE_HEADER,
    HEALTH_PROOF_HEADER,
};
pub use paths::WoofPaths;
pub use secure_file::{
    atomic_write_private, ensure_private_dir, private_file_mode, read_private_file_bounded,
    PrivateFileError,
};

pub const PRODUCT_NAME: &str = "woof";
pub const BUNDLE_ID: &str = "com.julius.woof";
pub const DEFAULT_BIND: &str = "127.0.0.1:3334";
