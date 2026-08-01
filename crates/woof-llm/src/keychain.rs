use std::fmt;

use zeroize::{Zeroize, Zeroizing};

use crate::KeyStoreError;

pub const OPENAI_KEYCHAIN_SERVICE: &str = "com.julius.woof.openai";
pub const OPENAI_KEYCHAIN_ACCOUNT: &str = "api-key";
const MAX_API_KEY_BYTES: usize = 4_096;

pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let mut value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty()
            || trimmed.len() > MAX_API_KEY_BYTES
            || trimmed.chars().any(char::is_control)
        {
            value.zeroize();
            return None;
        }
        let secret = trimmed.to_owned();
        value.zeroize();
        Some(Self(Zeroizing::new(secret)))
    }

    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl Clone for ApiKey {
    fn clone(&self) -> Self {
        Self(Zeroizing::new(self.0.to_string()))
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiKey([REDACTED])")
    }
}

pub trait OpenAiKeyStore: Send + Sync {
    fn get(&self) -> Result<ApiKey, KeyStoreError>;
    fn set(&self, key: &ApiKey) -> Result<(), KeyStoreError>;
    fn delete(&self) -> Result<(), KeyStoreError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MacOsKeychain;

#[cfg(target_os = "macos")]
impl MacOsKeychain {
    fn entry(&self) -> Result<keyring::Entry, KeyStoreError> {
        keyring::Entry::new(OPENAI_KEYCHAIN_SERVICE, OPENAI_KEYCHAIN_ACCOUNT)
            .map_err(|_| KeyStoreError::Unavailable)
    }
}

#[cfg(target_os = "macos")]
impl OpenAiKeyStore for MacOsKeychain {
    fn get(&self) -> Result<ApiKey, KeyStoreError> {
        let value = self.entry()?.get_password().map_err(|error| match error {
            keyring::Error::NoEntry => KeyStoreError::NotFound,
            _ => KeyStoreError::Access,
        })?;
        ApiKey::new(value).ok_or(KeyStoreError::NotFound)
    }

    fn set(&self, key: &ApiKey) -> Result<(), KeyStoreError> {
        self.entry()?
            .set_password(key.expose())
            .map_err(|_| KeyStoreError::Access)
    }

    fn delete(&self) -> Result<(), KeyStoreError> {
        self.entry()?
            .delete_credential()
            .map_err(|error| match error {
                keyring::Error::NoEntry => KeyStoreError::NotFound,
                _ => KeyStoreError::Access,
            })
    }
}

#[cfg(not(target_os = "macos"))]
impl OpenAiKeyStore for MacOsKeychain {
    fn get(&self) -> Result<ApiKey, KeyStoreError> {
        Err(KeyStoreError::Unavailable)
    }

    fn set(&self, _key: &ApiKey) -> Result<(), KeyStoreError> {
        Err(KeyStoreError::Unavailable)
    }

    fn delete(&self) -> Result<(), KeyStoreError> {
        Err(KeyStoreError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_exposes_key() {
        let key = ApiKey::new("sk-test-private").unwrap();
        let debug = format!("{key:?}");
        assert!(!debug.contains("sk-test-private"));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn keys_are_trimmed_and_bounded() {
        assert_eq!(
            ApiKey::new("  sk-test-private  ").unwrap().expose(),
            "sk-test-private"
        );
        assert!(ApiKey::new("sk-test\nprivate").is_none());
        assert!(ApiKey::new("x".repeat(MAX_API_KEY_BYTES + 1)).is_none());
    }
}
