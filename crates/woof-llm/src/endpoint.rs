use url::Url;

pub const OPENAI_HOST: &str = "api.openai.com";
pub const CHAT_COMPLETIONS_URL: &str = "https://api.openai.com/v1/chat/completions";
pub const REALTIME_TRANSCRIPTION_URL: &str =
    "wss://api.openai.com/v1/realtime?intent=transcription";

pub fn validate_openai_url(value: &str, websocket: bool) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let expected_scheme = if websocket { "wss" } else { "https" };
    url.scheme() == expected_scheme
        && url.host_str() == Some(OPENAI_HOST)
        && url.port().is_none_or(|port| port == 443)
        && url.username().is_empty()
        && url.password().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_lookalikes_credentials_and_plaintext() {
        assert!(validate_openai_url(CHAT_COMPLETIONS_URL, false));
        assert!(validate_openai_url(REALTIME_TRANSCRIPTION_URL, true));
        for invalid in [
            "http://api.openai.com/v1/chat/completions",
            "https://api.openai.com.evil.invalid/v1/chat/completions",
            "https://api.openai.com@evil.invalid/v1/chat/completions",
            "https://user:pass@api.openai.com/v1/chat/completions",
            "https://api.openai.com:8443/v1/chat/completions",
        ] {
            assert!(!validate_openai_url(invalid, false), "{invalid}");
        }
    }
}
