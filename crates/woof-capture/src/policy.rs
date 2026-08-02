use regex::Regex;
use serde::{Deserialize, Serialize};
use url::{Host, Url};
use zeroize::Zeroizing;

use crate::{CaptureMetadata, RawCapture, WOOF_BUNDLE_ID};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlacklistKind {
    BundleId,
    BundlePrefix,
    AppName,
    WindowTitle,
    BrowserHost,
    Regex,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlacklistRule {
    pub kind: BlacklistKind,
    pub pattern: String,
}

#[derive(Clone, Debug, Default)]
pub struct CapturePolicy {
    blacklist: Vec<CompiledRule>,
}

#[derive(Clone, Debug)]
struct CompiledRule {
    source: BlacklistRule,
    regex: Option<Regex>,
    browser_host: Option<Host<String>>,
}

impl CapturePolicy {
    pub fn new(rules: impl IntoIterator<Item = BlacklistRule>) -> Self {
        let blacklist = rules
            .into_iter()
            .map(|source| {
                let regex = matches!(source.kind, BlacklistKind::Regex)
                    .then(|| Regex::new(&source.pattern).ok())
                    .flatten();
                let browser_host = matches!(source.kind, BlacklistKind::BrowserHost)
                    .then(|| canonical_rule_host(&source.pattern))
                    .flatten();
                CompiledRule {
                    source,
                    regex,
                    browser_host,
                }
            })
            .collect();
        Self { blacklist }
    }

    pub fn is_blacklisted(&self, capture: &RawCapture) -> bool {
        self.is_blacklisted_fields(CaptureFields {
            bundle_id: capture.bundle_id.as_deref(),
            app_name: &capture.app_name,
            window_title: capture.window_title.as_deref(),
            browser_url: capture.browser_url.as_deref(),
        })
    }

    pub fn is_blacklisted_metadata(&self, metadata: &CaptureMetadata) -> bool {
        self.is_blacklisted_metadata_with_browser_url(metadata, metadata.browser_url.as_deref())
    }

    pub(crate) fn is_blacklisted_metadata_with_browser_url(
        &self,
        metadata: &CaptureMetadata,
        browser_url: Option<&str>,
    ) -> bool {
        self.is_blacklisted_fields(CaptureFields {
            bundle_id: metadata.bundle_id.as_deref(),
            app_name: &metadata.app_name,
            window_title: metadata.window_title.as_deref(),
            browser_url,
        })
    }

    /// Browser-host rules and arbitrary metadata regexes cannot be evaluated
    /// safely when a browser exposes a web document without its URL.
    pub fn requires_browser_url_preflight(&self) -> bool {
        self.blacklist.iter().any(|rule| {
            matches!(
                rule.source.kind,
                BlacklistKind::BrowserHost | BlacklistKind::Regex
            )
        })
    }

    fn is_blacklisted_fields(&self, fields: CaptureFields<'_>) -> bool {
        if fields.bundle_id == Some(WOOF_BUNDLE_ID) {
            return true;
        }

        self.blacklist.iter().any(|rule| rule.matches(fields))
    }
}

#[derive(Clone, Copy)]
struct CaptureFields<'a> {
    bundle_id: Option<&'a str>,
    app_name: &'a str,
    window_title: Option<&'a str>,
    browser_url: Option<&'a str>,
}

impl CompiledRule {
    fn matches(&self, fields: CaptureFields<'_>) -> bool {
        let pattern = self.source.pattern.to_ascii_lowercase();
        match self.source.kind {
            BlacklistKind::BundleId => fields
                .bundle_id
                .is_some_and(|value| value.eq_ignore_ascii_case(&self.source.pattern)),
            BlacklistKind::BundlePrefix => fields
                .bundle_id
                .is_some_and(|value| value.to_ascii_lowercase().starts_with(&pattern)),
            BlacklistKind::AppName => {
                Zeroizing::new(fields.app_name.to_ascii_lowercase()).contains(&pattern)
            }
            BlacklistKind::WindowTitle => fields
                .window_title
                .is_some_and(|value| Zeroizing::new(value.to_ascii_lowercase()).contains(&pattern)),
            BlacklistKind::BrowserHost => fields
                .browser_url
                .and_then(canonical_url_host)
                .is_some_and(|host| {
                    self.browser_host
                        .as_ref()
                        .is_some_and(|pattern| hosts_match(&host, pattern))
                }),
            BlacklistKind::Regex => self.regex.as_ref().is_some_and(|regex| {
                let joined = Zeroizing::new(format!(
                    "{}\n{}\n{}\n{}",
                    fields.bundle_id.unwrap_or_default(),
                    fields.app_name,
                    fields.window_title.unwrap_or_default(),
                    fields.browser_url.unwrap_or_default()
                ));
                regex.is_match(&joined)
            }),
        }
    }
}

fn canonical_rule_host(value: &str) -> Option<Host<String>> {
    let value = value.trim().trim_end_matches('.');
    if value.is_empty() {
        return None;
    }
    if value.contains(':') && !(value.starts_with('[') && value.ends_with(']')) {
        Host::parse(&format!("[{value}]")).ok()
    } else {
        Host::parse(value).ok()
    }
}

fn canonical_url_host(value: &str) -> Option<Host<String>> {
    match Url::parse(value.trim()).ok()?.host()? {
        Host::Domain(domain) => Host::parse(domain.trim_end_matches('.')).ok(),
        host => Some(host.to_owned()),
    }
}

fn hosts_match(host: &Host<String>, pattern: &Host<String>) -> bool {
    if host == pattern {
        return true;
    }
    match (host, pattern) {
        (Host::Domain(host), Host::Domain(pattern)) => host
            .strip_suffix(pattern)
            .is_some_and(|prefix| prefix.ends_with('.')),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::AccessibilityNode;

    use super::*;

    fn capture() -> RawCapture {
        RawCapture {
            captured_at_ms: 0,
            pid: 1,
            app_name: "Safari".into(),
            bundle_id: Some("com.apple.Safari".into()),
            window_title: Some("Private payroll".into()),
            window_id: None,
            browser_url: Some("https://secret.example.com/report".into()),
            secure_input: false,
            root: AccessibilityNode::default(),
        }
    }

    #[test]
    fn matches_browser_subdomains() {
        let policy = CapturePolicy::new([BlacklistRule {
            kind: BlacklistKind::BrowserHost,
            pattern: "example.com".into(),
        }]);
        assert!(policy.is_blacklisted(&capture()));
    }

    #[test]
    fn canonicalizes_trailing_dots_and_ipv6_hosts() {
        let mut value = capture();
        value.browser_url = Some("https://secret.example.com./report".into());
        let domain_policy = CapturePolicy::new([BlacklistRule {
            kind: BlacklistKind::BrowserHost,
            pattern: "example.com".into(),
        }]);
        assert!(domain_policy.is_blacklisted(&value));

        value.browser_url = Some("https://[2001:db8::1]/report".into());
        for pattern in ["[2001:db8::1]", "2001:0db8:0:0:0:0:0:1"] {
            let ipv6_policy = CapturePolicy::new([BlacklistRule {
                kind: BlacklistKind::BrowserHost,
                pattern: pattern.into(),
            }]);
            assert!(ipv6_policy.is_blacklisted(&value), "pattern {pattern}");
        }
    }

    #[test]
    fn always_excludes_woof() {
        let mut value = capture();
        value.bundle_id = Some(WOOF_BUNDLE_ID.into());
        assert!(CapturePolicy::default().is_blacklisted(&value));
    }

    #[test]
    fn host_and_regex_rules_require_a_known_browser_url() {
        for kind in [BlacklistKind::BrowserHost, BlacklistKind::Regex] {
            let policy = CapturePolicy::new([BlacklistRule {
                kind,
                pattern: "example".into(),
            }]);
            assert!(policy.requires_browser_url_preflight());
        }
        let app_policy = CapturePolicy::new([BlacklistRule {
            kind: BlacklistKind::AppName,
            pattern: "browser".into(),
        }]);
        assert!(!app_policy.requires_browser_url_preflight());
    }
}
