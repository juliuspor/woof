use std::{fs, path::Path};

use rand::RngCore;
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroize;

use crate::{atomic_write_private, read_private_file_bounded, PrivateFileError};

/// Bearer token file length. The file contains lowercase hexadecimal bytes.
pub const TOKEN_BYTES: usize = 64;

#[derive(Clone)]
pub struct ApiToken([u8; TOKEN_BYTES]);

impl Drop for ApiToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for ApiToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ApiToken([REDACTED])")
    }
}

#[derive(Debug, Error)]
pub enum ApiTokenError {
    #[error(transparent)]
    PrivateFile(#[from] PrivateFileError),
    #[error("token at {0} must contain exactly 64 lowercase hexadecimal bytes")]
    Invalid(std::path::PathBuf),
    #[error("failed to read token at {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl ApiToken {
    pub fn generate() -> Self {
        let mut entropy = [0_u8; TOKEN_BYTES / 2];
        rand::rng().fill_bytes(&mut entropy);
        let encoded = hex::encode(entropy);
        entropy.zeroize();
        let mut token = [0_u8; TOKEN_BYTES];
        token.copy_from_slice(encoded.as_bytes());
        Self(token)
    }

    pub fn load_or_create(path: &Path) -> Result<Self, ApiTokenError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(PrivateFileError::Symlink(path.to_path_buf()).into())
            }
            Ok(metadata) if !metadata.is_file() => {
                Err(PrivateFileError::NotRegularFile(path.to_path_buf()).into())
            }
            Ok(_) => {
                let bytes = read_private_file_bounded(path, TOKEN_BYTES)?;
                Self::parse_file(path, bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let token = Self::generate();
                atomic_write_private(path, token.expose())?;
                Ok(token)
            }
            Err(source) => Err(ApiTokenError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Loads the private bearer token, replacing only malformed contents.
    /// Unsafe path types and I/O failures remain terminal rather than being
    /// overwritten.
    pub fn load_or_replace_invalid(path: &Path) -> Result<Self, ApiTokenError> {
        match Self::load_or_create(path) {
            Ok(token) => Ok(token),
            Err(ApiTokenError::Invalid(found)) if found == path => {
                let token = Self::generate();
                atomic_write_private(path, token.expose())?;
                Ok(token)
            }
            Err(error) => Err(error),
        }
    }

    pub fn parse_file(path: &Path, mut bytes: Vec<u8>) -> Result<Self, ApiTokenError> {
        let valid = bytes.len() == TOKEN_BYTES
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte));
        if !valid {
            bytes.zeroize();
            return Err(ApiTokenError::Invalid(path.to_path_buf()));
        }
        let mut token = [0_u8; TOKEN_BYTES];
        token.copy_from_slice(&bytes);
        bytes.zeroize();
        Ok(Self(token))
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn expose_str(&self) -> &str {
        // Construction validates or emits ASCII hexadecimal.
        std::str::from_utf8(&self.0).expect("API token is hexadecimal ASCII")
    }

    pub fn matches_bearer(&self, candidate: &[u8]) -> bool {
        let mut fixed = [0_u8; TOKEN_BYTES];
        let copied = candidate.len().min(TOKEN_BYTES);
        fixed[..copied].copy_from_slice(&candidate[..copied]);
        let length_matches = (candidate.len() as u64).ct_eq(&(TOKEN_BYTES as u64));
        let value_matches = self.0.ct_eq(&fixed);
        fixed.zeroize();
        (length_matches & value_matches).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_fixed_length_hex_and_compare_constant_time() {
        let token = ApiToken::generate();
        assert_eq!(token.expose().len(), TOKEN_BYTES);
        assert!(token.expose().iter().all(u8::is_ascii_hexdigit));
        assert!(token.matches_bearer(token.expose()));
        assert!(!token.matches_bearer(b"short"));
        assert!(!token.matches_bearer(&[b'a'; TOKEN_BYTES + 1]));
    }
}
