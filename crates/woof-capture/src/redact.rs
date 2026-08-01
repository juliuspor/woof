use std::{fmt, sync::LazyLock};

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,63}\b").expect("valid email regex")
});
static IBAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[A-Z]{2}[0-9]{2}(?:[ ]?[A-Z0-9]){11,30}\b").expect("valid IBAN regex")
});
static SSN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{3}[- ]\d{2}[- ]\d{4}\b").expect("valid SSN regex"));
static CARD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:\d[ -]?){12,18}\d\b").expect("valid payment card regex"));
static CVV: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(cvv2?|cvc2?|cid|security[ -]?code)(\s*(?:is|:|=)?\s*)(\d{3,4})\b")
        .expect("valid CVV regex")
});
static PHONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        (?:
          \+\d{1,3}[\s.-]?
          (?:\(\d{1,4}\)|\d{1,4})
          (?:[\s.-]?\d{2,4}){2,4}
        )
        |
        (?:
          \(\d{3}\)[\s.-]?\d{3}[\s.-]?\d{4}
        )
        |
        (?:
          \b\d{3}[-.]\d{3}[-.]\d{4}\b
        )",
    )
    .expect("valid phone regex")
});

static PEM_PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?s)-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----.*?-----END (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----",
    )
    .expect("valid PEM private key regex")
});
static AUTHORIZATION_BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(authorization[ \t]*:[ \t]*bearer[ \t]+)([A-Za-z0-9._~+/=-]{16,})")
        .expect("valid authorization bearer regex")
});
static PROVIDER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        \b(?:
          sk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{20,}
          |
          github_pat_[A-Za-z0-9_]{20,}
          |
          gh[pousr]_[A-Za-z0-9]{20,}
          |
          xox[baprs]-[A-Za-z0-9-]{20,}
          |
          xapp-[A-Za-z0-9-]{20,}
        )",
    )
    .expect("valid provider token regex")
});
static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\beyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}")
        .expect("valid JWT regex")
});
static LABELED_SECRET_DOUBLE_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        (
          \b(?:
            openai[_-]?api[_-]?key
            | api[_-]?key
            | access[_-]?token
            | refresh[_-]?token
            | auth[_-]?token
            | bearer[_-]?token
            | client[_-]?secret
            | secret[_-]?access[_-]?key
            | aws[_-]?access[_-]?key[_-]?id
            | aws[_-]?secret[_-]?access[_-]?key
            | aws[_-]?session[_-]?token
            | github[_-]?(?:token|pat)
            | password
            | passwd
          )["']?[\x20\t]*(?:=|:)[\x20\t]*"
        )
        ((?:\\.|[^"\\\r\n])+)
        (")"#,
    )
    .expect("valid double-quoted labeled secret regex")
});
static LABELED_SECRET_SINGLE_QUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        (
          \b(?:
            openai[_-]?api[_-]?key
            | api[_-]?key
            | access[_-]?token
            | refresh[_-]?token
            | auth[_-]?token
            | bearer[_-]?token
            | client[_-]?secret
            | secret[_-]?access[_-]?key
            | aws[_-]?access[_-]?key[_-]?id
            | aws[_-]?secret[_-]?access[_-]?key
            | aws[_-]?session[_-]?token
            | github[_-]?(?:token|pat)
            | password
            | passwd
          )["']?[\x20\t]*(?:=|:)[\x20\t]*'
        )
        ((?:\\.|[^'\\\r\n])+)
        (')"#,
    )
    .expect("valid single-quoted labeled secret regex")
});
static LABELED_SECRET_UNQUOTED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?ix)
        (
          \b(?:
            openai[_-]?api[_-]?key
            | api[_-]?key
            | access[_-]?token
            | refresh[_-]?token
            | auth[_-]?token
            | bearer[_-]?token
            | client[_-]?secret
            | secret[_-]?access[_-]?key
            | aws[_-]?access[_-]?key[_-]?id
            | aws[_-]?secret[_-]?access[_-]?key
            | aws[_-]?session[_-]?token
            | github[_-]?(?:token|pat)
            | password
            | passwd
          )["']?[\x20\t]*(?:=|:)[\x20\t]*
        )
        ([^\x20\t"'\r\n][^\r\n]*)"#,
    )
    .expect("valid unquoted labeled secret regex")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionKind {
    Credential,
    Email,
    Iban,
    SocialSecurityNumber,
    PaymentCard,
    CardSecurityCode,
    Phone,
}

impl RedactionKind {
    fn marker(self) -> &'static str {
        match self {
            Self::Credential => "[REDACTED_CREDENTIAL]",
            Self::Email => "[REDACTED_EMAIL]",
            Self::Iban => "[REDACTED_IBAN]",
            Self::SocialSecurityNumber => "[REDACTED_SSN]",
            Self::PaymentCard => "[REDACTED_CARD]",
            Self::CardSecurityCode => "[REDACTED_CVV]",
            Self::Phone => "[REDACTED_PHONE]",
        }
    }

    fn marker_name(self) -> &'static str {
        match self {
            Self::Credential => "CREDENTIAL",
            Self::Email => "EMAIL",
            Self::Iban => "IBAN",
            Self::SocialSecurityNumber => "SSN",
            Self::PaymentCard => "CARD",
            Self::CardSecurityCode => "CVV",
            Self::Phone => "PHONE",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionReport {
    pub text: String,
    pub counts: Vec<(RedactionKind, usize)>,
}

impl RedactionReport {
    pub fn total(&self) -> usize {
        self.counts.iter().map(|(_, count)| count).sum()
    }

    pub fn count(&self, kind: RedactionKind) -> usize {
        self.counts
            .iter()
            .find_map(|(candidate, count)| (*candidate == kind).then_some(*count))
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Redactor {
    // Keep construction explicit through `Default` without carrying a runtime
    // switch that could disable mandatory redaction.
    _private: (),
}

/// A redacted prompt representation whose opaque markers can be restored
/// locally after model output has been validated.
///
/// The original values are intentionally private and its `Debug`
/// implementation never exposes text or marker contents.
pub struct RestorableRedaction {
    text: String,
    nonce: String,
    slots: Vec<RedactionSlot>,
}

struct RedactionSlot {
    marker: String,
    value: String,
}

impl Drop for RedactionSlot {
    fn drop(&mut self) {
        self.marker.zeroize();
        self.value.zeroize();
    }
}

impl fmt::Debug for RestorableRedaction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RestorableRedaction")
            .field("text", &"[REDACTED]")
            .field("utf8_bytes", &self.text.len())
            .field("slot_count", &self.slots.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RedactionRestoreError {
    #[error("a private placeholder is missing from the rewritten text")]
    MissingPlaceholder,
    #[error("a private placeholder was duplicated in the rewritten text")]
    DuplicatedPlaceholder,
    #[error("the rewritten text contains a malformed private placeholder")]
    UnexpectedPlaceholder,
}

impl RestorableRedaction {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn redaction_count(&self) -> usize {
        self.slots.len()
    }

    /// Restores each original value only when the model preserved every
    /// request-specific marker exactly once. This prevents deletion,
    /// duplication, or fabrication of private values during a rewrite.
    pub fn restore(&self, output: &str) -> Result<String, RedactionRestoreError> {
        let mut restored = output.to_owned();
        for slot in &self.slots {
            match restored.match_indices(&slot.marker).count() {
                0 => return Err(RedactionRestoreError::MissingPlaceholder),
                1 => {
                    restored = restored.replacen(&slot.marker, &slot.value, 1);
                }
                _ => return Err(RedactionRestoreError::DuplicatedPlaceholder),
            }
        }
        let marker_prefix = format!("[WOOF_REDACTED_{}_", self.nonce);
        if restored.contains(&marker_prefix) {
            return Err(RedactionRestoreError::UnexpectedPlaceholder);
        }
        Ok(restored)
    }
}

impl Redactor {
    pub fn redact(&self, input: &str) -> RedactionReport {
        let mut text = input.to_owned();
        let mut counts = Vec::new();
        redact_credentials(&mut text, &mut counts);
        replace_simple(&mut text, RedactionKind::Email, &EMAIL, &mut counts);
        replace_validated(
            &mut text,
            RedactionKind::Iban,
            &IBAN,
            is_valid_iban,
            &mut counts,
        );
        replace_validated(
            &mut text,
            RedactionKind::SocialSecurityNumber,
            &SSN,
            is_valid_ssn,
            &mut counts,
        );
        replace_validated(
            &mut text,
            RedactionKind::PaymentCard,
            &CARD,
            is_valid_card_number,
            &mut counts,
        );

        let mut cvv_count = 0;
        text = CVV
            .replace_all(&text, |captures: &Captures<'_>| {
                cvv_count += 1;
                format!(
                    "{}{}{}",
                    captures.get(1).map_or("", |value| value.as_str()),
                    captures.get(2).map_or("", |value| value.as_str()),
                    RedactionKind::CardSecurityCode.marker()
                )
            })
            .into_owned();
        if cvv_count > 0 {
            counts.push((RedactionKind::CardSecurityCode, cvv_count));
        }
        replace_simple(&mut text, RedactionKind::Phone, &PHONE, &mut counts);

        RedactionReport { text, counts }
    }

    /// Redacts private values using unique opaque placeholders that can be
    /// restored locally after an untrusted rewrite service returns text.
    pub fn redact_restorable(&self, input: &str) -> RestorableRedaction {
        let nonce = unique_nonce(input);
        let mut text = input.to_owned();
        let mut slots = Vec::new();
        redact_credentials_restorable(&mut text, &nonce, &mut slots);
        replace_restorable(
            &mut text,
            RedactionKind::Email,
            &EMAIL,
            |_| true,
            &nonce,
            &mut slots,
        );
        replace_restorable(
            &mut text,
            RedactionKind::Iban,
            &IBAN,
            is_valid_iban,
            &nonce,
            &mut slots,
        );
        replace_restorable(
            &mut text,
            RedactionKind::SocialSecurityNumber,
            &SSN,
            is_valid_ssn,
            &nonce,
            &mut slots,
        );
        replace_restorable(
            &mut text,
            RedactionKind::PaymentCard,
            &CARD,
            is_valid_card_number,
            &nonce,
            &mut slots,
        );
        text = CVV
            .replace_all(&text, |captures: &Captures<'_>| {
                let value = captures.get(3).map_or("", |value| value.as_str());
                let marker = push_slot(&mut slots, &nonce, RedactionKind::CardSecurityCode, value);
                format!(
                    "{}{}{}",
                    captures.get(1).map_or("", |value| value.as_str()),
                    captures.get(2).map_or("", |value| value.as_str()),
                    marker
                )
            })
            .into_owned();
        replace_restorable(
            &mut text,
            RedactionKind::Phone,
            &PHONE,
            |_| true,
            &nonce,
            &mut slots,
        );

        RestorableRedaction { text, nonce, slots }
    }
}

fn redact_credentials(text: &mut String, counts: &mut Vec<(RedactionKind, usize)>) {
    replace_simple(text, RedactionKind::Credential, &PEM_PRIVATE_KEY, counts);
    replace_group_validated(
        text,
        RedactionKind::Credential,
        &AUTHORIZATION_BEARER,
        2,
        |_| true,
        counts,
    );
    replace_group_validated(
        text,
        RedactionKind::Credential,
        &LABELED_SECRET_DOUBLE_QUOTED,
        2,
        is_plausible_labeled_secret,
        counts,
    );
    replace_group_validated(
        text,
        RedactionKind::Credential,
        &LABELED_SECRET_SINGLE_QUOTED,
        2,
        is_plausible_labeled_secret,
        counts,
    );
    replace_group_validated(
        text,
        RedactionKind::Credential,
        &LABELED_SECRET_UNQUOTED,
        2,
        is_plausible_labeled_secret,
        counts,
    );
    replace_simple(text, RedactionKind::Credential, &PROVIDER_TOKEN, counts);
    replace_simple(text, RedactionKind::Credential, &JWT, counts);
}

fn redact_credentials_restorable(text: &mut String, nonce: &str, slots: &mut Vec<RedactionSlot>) {
    replace_restorable(
        text,
        RedactionKind::Credential,
        &PEM_PRIVATE_KEY,
        |_| true,
        nonce,
        slots,
    );
    replace_group_restorable(
        text,
        RedactionKind::Credential,
        &AUTHORIZATION_BEARER,
        2,
        |_| true,
        nonce,
        slots,
    );
    replace_group_restorable(
        text,
        RedactionKind::Credential,
        &LABELED_SECRET_DOUBLE_QUOTED,
        2,
        is_plausible_labeled_secret,
        nonce,
        slots,
    );
    replace_group_restorable(
        text,
        RedactionKind::Credential,
        &LABELED_SECRET_SINGLE_QUOTED,
        2,
        is_plausible_labeled_secret,
        nonce,
        slots,
    );
    replace_group_restorable(
        text,
        RedactionKind::Credential,
        &LABELED_SECRET_UNQUOTED,
        2,
        is_plausible_labeled_secret,
        nonce,
        slots,
    );
    replace_restorable(
        text,
        RedactionKind::Credential,
        &PROVIDER_TOKEN,
        |_| true,
        nonce,
        slots,
    );
    replace_restorable(
        text,
        RedactionKind::Credential,
        &JWT,
        |_| true,
        nonce,
        slots,
    );
}

fn unique_nonce(input: &str) -> String {
    loop {
        let nonce = Uuid::new_v4().simple().to_string();
        if !input.contains(&format!("[WOOF_REDACTED_{nonce}_")) {
            return nonce;
        }
    }
}

fn push_slot(
    slots: &mut Vec<RedactionSlot>,
    nonce: &str,
    kind: RedactionKind,
    value: &str,
) -> String {
    let marker = format!(
        "[WOOF_REDACTED_{}_{}_{}]",
        nonce,
        kind.marker_name(),
        slots.len()
    );
    slots.push(RedactionSlot {
        marker: marker.clone(),
        value: value.to_owned(),
    });
    marker
}

fn replace_restorable(
    text: &mut String,
    kind: RedactionKind,
    regex: &Regex,
    validate: fn(&str) -> bool,
    nonce: &str,
    slots: &mut Vec<RedactionSlot>,
) {
    *text = regex
        .replace_all(text, |captures: &Captures<'_>| {
            let candidate = captures.get(0).map_or("", |value| value.as_str());
            if validate(candidate) {
                push_slot(slots, nonce, kind, candidate)
            } else {
                candidate.to_owned()
            }
        })
        .into_owned();
}

fn replace_group_restorable(
    text: &mut String,
    kind: RedactionKind,
    regex: &Regex,
    capture_group: usize,
    validate: fn(&str) -> bool,
    nonce: &str,
    slots: &mut Vec<RedactionSlot>,
) {
    *text = regex
        .replace_all(text, |captures: &Captures<'_>| {
            let Some(whole) = captures.get(0) else {
                return String::new();
            };
            let Some(candidate) = captures.get(capture_group) else {
                return whole.as_str().to_owned();
            };
            if !validate(candidate.as_str()) {
                return whole.as_str().to_owned();
            }
            let start = candidate.start().saturating_sub(whole.start());
            let end = candidate.end().saturating_sub(whole.start());
            let marker = push_slot(slots, nonce, kind, candidate.as_str());
            format!(
                "{}{}{}",
                &whole.as_str()[..start],
                marker,
                &whole.as_str()[end..]
            )
        })
        .into_owned();
}

fn replace_simple(
    text: &mut String,
    kind: RedactionKind,
    regex: &Regex,
    counts: &mut Vec<(RedactionKind, usize)>,
) {
    let mut count = 0;
    *text = regex
        .replace_all(text, |_: &Captures<'_>| {
            count += 1;
            kind.marker()
        })
        .into_owned();
    if count > 0 {
        record_count(counts, kind, count);
    }
}

fn replace_group_validated(
    text: &mut String,
    kind: RedactionKind,
    regex: &Regex,
    capture_group: usize,
    validate: fn(&str) -> bool,
    counts: &mut Vec<(RedactionKind, usize)>,
) {
    let mut count = 0;
    *text = regex
        .replace_all(text, |captures: &Captures<'_>| {
            let Some(whole) = captures.get(0) else {
                return String::new();
            };
            let Some(candidate) = captures.get(capture_group) else {
                return whole.as_str().to_owned();
            };
            if !validate(candidate.as_str()) {
                return whole.as_str().to_owned();
            }
            count += 1;
            let start = candidate.start().saturating_sub(whole.start());
            let end = candidate.end().saturating_sub(whole.start());
            format!(
                "{}{}{}",
                &whole.as_str()[..start],
                kind.marker(),
                &whole.as_str()[end..]
            )
        })
        .into_owned();
    record_count(counts, kind, count);
}

fn replace_validated(
    text: &mut String,
    kind: RedactionKind,
    regex: &Regex,
    validate: fn(&str) -> bool,
    counts: &mut Vec<(RedactionKind, usize)>,
) {
    let mut count = 0;
    *text = regex
        .replace_all(text, |captures: &Captures<'_>| {
            let candidate = captures.get(0).map_or("", |value| value.as_str());
            if validate(candidate) {
                count += 1;
                kind.marker().to_owned()
            } else {
                candidate.to_owned()
            }
        })
        .into_owned();
    if count > 0 {
        record_count(counts, kind, count);
    }
}

fn record_count(counts: &mut Vec<(RedactionKind, usize)>, kind: RedactionKind, count: usize) {
    if count == 0 {
        return;
    }
    if let Some((_, existing)) = counts.iter_mut().find(|(candidate, _)| *candidate == kind) {
        *existing = existing.saturating_add(count);
    } else {
        counts.push((kind, count));
    }
}

fn is_plausible_labeled_secret(candidate: &str) -> bool {
    let normalized = candidate.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    ![
        "changeme",
        "change-me",
        "change_me",
        "not-a-secret",
        "not_a_secret",
        "redacted",
        "[redacted]",
        "<redacted>",
        "example",
        "placeholder",
        "replace-me",
        "replace_me",
        "your password here",
        "your-password-here",
        "your_password_here",
        "your api key here",
        "your-api-key-here",
        "your_api_key_here",
        "akiaiosfodnn7example",
    ]
    .iter()
    .any(|placeholder| normalized == *placeholder)
}

fn is_valid_card_number(candidate: &str) -> bool {
    let digits: Vec<u32> = candidate
        .chars()
        .filter_map(|value| value.to_digit(10))
        .collect();
    if !(13..=19).contains(&digits.len()) || digits.iter().all(|digit| *digit == digits[0]) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(index, digit)| {
            if index % 2 == 1 {
                let doubled = digit * 2;
                if doubled > 9 {
                    doubled - 9
                } else {
                    doubled
                }
            } else {
                *digit
            }
        })
        .sum();
    sum % 10 == 0
}

fn is_valid_ssn(candidate: &str) -> bool {
    let digits: String = candidate.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 9 {
        return false;
    }
    let area = &digits[..3];
    let group = &digits[3..5];
    let serial = &digits[5..];
    area != "000" && area != "666" && !area.starts_with('9') && group != "00" && serial != "0000"
}

fn is_valid_iban(candidate: &str) -> bool {
    let compact: String = candidate
        .chars()
        .filter(|value| !value.is_ascii_whitespace())
        .map(|value| value.to_ascii_uppercase())
        .collect();
    if !(15..=34).contains(&compact.len())
        || !compact
            .chars()
            .take(2)
            .all(|value| value.is_ascii_alphabetic())
        || !compact
            .chars()
            .skip(2)
            .take(2)
            .all(|value| value.is_ascii_digit())
        || !compact.chars().all(|value| value.is_ascii_alphanumeric())
    {
        return false;
    }

    let rearranged = format!("{}{}", &compact[4..], &compact[..4]);
    let mut remainder = 0_u32;
    for value in rearranged.chars() {
        if let Some(digit) = value.to_digit(10) {
            remainder = (remainder * 10 + digit) % 97;
        } else {
            let encoded = value as u32 - 'A' as u32 + 10;
            remainder = (remainder * 100 + encoded) % 97;
        }
    }
    remainder == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_all_supported_categories() {
        let input = concat!(
            "Email jane.doe@example.com; IBAN DE89 3704 0044 0532 0130 00; ",
            "SSN 123-45-6789; Visa 4111 1111 1111 1111; CVV: 123; ",
            "phone +49 30 1234 5678."
        );
        let report = Redactor::default().redact(input);
        for marker in [
            "[REDACTED_EMAIL]",
            "[REDACTED_IBAN]",
            "[REDACTED_SSN]",
            "[REDACTED_CARD]",
            "[REDACTED_CVV]",
            "[REDACTED_PHONE]",
        ] {
            assert!(
                report.text.contains(marker),
                "missing {marker}: {}",
                report.text
            );
        }
        assert_eq!(report.total(), 6);
    }

    #[test]
    fn redacts_high_confidence_credentials() {
        let bearer = "AbCdEfGhIjKlMnOpQrStUvWxYz012345";
        let openai = "sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz012345";
        let github = "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789";
        let slack = "xoxb-AbCdEfGhIjKlMnOpQrStUvWxYz012345";
        let slack_app = "xapp-1-AbCdEfGhIjKlMnOpQrStUvWxYz012345";
        let aws = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789ABCD";
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmaXh0dXJlIn0.ZmFrZXNpZ25hdHVyZQ";
        let client_secret = "AbCdEfGhIjKlMnOpQrStUvWxYz987654";
        let access_token = "ZyXwVuTsRqPoNmLkJiHgFeDcBa654321";
        let pem = concat!(
            "-----BEGIN PRIVATE KEY-----\n",
            "U1lOVEhFVElDX0ZJWFRVUkVfTk9UX0FfS0VZ\n",
            "-----END PRIVATE KEY-----"
        );
        let input = format!(
            concat!(
                "Authorization: Bearer {bearer}\n",
                "OpenAI {openai}\nGitHub {github}\nSlack {slack}\nSlack app {slack_app}\n",
                "AWS_SECRET_ACCESS_KEY={aws}\n",
                "JWT {jwt}\nclient_secret={client_secret}\n",
                "\"access_token\": \"{access_token}\"\n{pem}"
            ),
            bearer = bearer,
            openai = openai,
            github = github,
            slack = slack,
            slack_app = slack_app,
            aws = aws,
            jwt = jwt,
            client_secret = client_secret,
            access_token = access_token,
            pem = pem,
        );

        let report = Redactor::default().redact(&input);
        assert_eq!(
            report.count(RedactionKind::Credential),
            10,
            "redacted output: {}",
            report.text
        );
        assert_eq!(report.text.matches("[REDACTED_CREDENTIAL]").count(), 10);
        for value in [
            bearer,
            openai,
            github,
            slack,
            slack_app,
            aws,
            jwt,
            client_secret,
            access_token,
            pem,
        ] {
            assert!(!report.text.contains(value), "credential survived: {value}");
        }
    }

    #[test]
    fn restorable_redaction_handles_credentials_and_restores_them_locally() {
        let input = concat!(
            "Authorization: Bearer AbCdEfGhIjKlMnOpQrStUvWxYz012345\n",
            "api_key=AbCdEfGhIjKlMnOpQrStUvWxYz987654\n",
            "JWT eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJmaXh0dXJlIn0.ZmFrZXNpZ25hdHVyZQ\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "U1lOVEhFVElDX0ZJWFRVUkVfTk9UX0FfS0VZ\n",
            "-----END PRIVATE KEY-----"
        );
        let redacted = Redactor::default().redact_restorable(input);
        assert_eq!(redacted.redaction_count(), 4);
        assert!(!redacted.text().contains("AbCdEfGhIjKlMnOp"));
        assert!(!redacted.text().contains("eyJhbGciOiJIUzI1NiJ9"));
        assert!(!redacted.text().contains("BEGIN PRIVATE KEY"));

        assert_eq!(redacted.restore(redacted.text()).unwrap(), input);
    }

    #[test]
    fn labeled_passphrases_are_redacted_without_entropy_heuristics() {
        let input = concat!(
            "password=\"correct horse battery staple\"\n",
            "client_secret='colon:value with spaces'\n",
            "api_key=aaaaaaaaaaaa\n",
            "refresh_token=prefix-example-real-token"
        );

        let report = Redactor::default().redact(input);
        assert_eq!(report.count(RedactionKind::Credential), 4);
        assert_eq!(report.text.matches("[REDACTED_CREDENTIAL]").count(), 4);
        for secret in [
            "correct horse battery staple",
            "colon:value with spaces",
            "aaaaaaaaaaaa",
            "prefix-example-real-token",
        ] {
            assert!(
                !report.text.contains(secret),
                "credential survived: {secret}"
            );
        }

        let restorable = Redactor::default().redact_restorable(input);
        assert_eq!(restorable.redaction_count(), 4);
        assert_eq!(restorable.restore(restorable.text()).unwrap(), input);
    }

    #[test]
    fn labeled_values_are_redacted_past_four_kibibytes() {
        let quoted_secret = "q".repeat(5_000);
        let unquoted_secret = "u".repeat(5_001);
        let input = format!("password=\"{quoted_secret}\"\napi_key={unquoted_secret}",);

        let report = Redactor::default().redact(&input);
        assert_eq!(report.count(RedactionKind::Credential), 2);
        assert_eq!(report.text.matches("[REDACTED_CREDENTIAL]").count(), 2);
        assert!(!report.text.contains(&quoted_secret));
        assert!(!report.text.contains(&unquoted_secret));

        let restorable = Redactor::default().redact_restorable(&input);
        assert_eq!(restorable.redaction_count(), 2);
        assert_eq!(restorable.restore(restorable.text()).unwrap(), input);
    }

    #[test]
    fn unquoted_labeled_values_do_not_leak_punctuation_suffixes() {
        let input = "password=abc#VerySecretSuffix,semi;brace}]";

        let report = Redactor::default().redact(input);
        assert_eq!(report.count(RedactionKind::Credential), 1);
        assert_eq!(report.text, "password=[REDACTED_CREDENTIAL]");
        assert!(!report.text.contains("VerySecretSuffix"));

        let restorable = Redactor::default().redact_restorable(input);
        assert_eq!(restorable.redaction_count(), 1);
        assert_eq!(restorable.restore(restorable.text()).unwrap(), input);
    }

    #[test]
    fn credential_detection_preserves_common_placeholders_and_prose() {
        let input = concat!(
            "Authorization: Bearer authentication is required.\n",
            "Examples: sk-example and ghp_example.\n",
            "api_key=your_api_key_here\n",
            "password=your_password_here\n",
            "JWT-like eyJheader.payload only.\n",
            "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n",
            "-----BEGIN PUBLIC KEY-----\nU1lOVEhFVElD\n-----END PUBLIC KEY-----"
        );
        let report = Redactor::default().redact(input);
        assert_eq!(report.text, input);
        assert_eq!(report.count(RedactionKind::Credential), 0);

        let restorable = Redactor::default().redact_restorable(input);
        assert_eq!(restorable.text(), input);
        assert_eq!(restorable.redaction_count(), 0);
    }

    #[test]
    fn preserves_invalid_card_and_iban_candidates() {
        let input = "order 4111 1111 1111 1112 and DE00 3704 0044 0532 0130 00";
        let report = Redactor::default().redact(input);
        assert_eq!(report.text, input);
        assert_eq!(report.total(), 0);
    }

    #[test]
    fn cvv_requires_a_context_label() {
        let report = Redactor::default().redact("There are 123 dogs and security code 987");
        assert!(report.text.starts_with("There are 123 dogs"));
        assert!(report.text.ends_with("[REDACTED_CVV]"));
    }

    #[test]
    fn restorable_redaction_keeps_private_values_out_of_prompt_and_restores_locally() {
        let input = "Email jane@example.com or call +49 30 1234 5678.";
        let redacted = Redactor::default().redact_restorable(input);
        assert_eq!(redacted.redaction_count(), 2);
        assert!(!redacted.text().contains("jane@example.com"));
        assert!(!redacted.text().contains("+49 30 1234 5678"));

        let output = format!("Please {} Thanks.", redacted.text());
        let restored = redacted.restore(&output).unwrap();
        assert!(restored.contains("jane@example.com"));
        assert!(restored.contains("+49 30 1234 5678"));
    }

    #[test]
    fn restorable_redaction_rejects_missing_or_duplicated_markers() {
        let redacted = Redactor::default().redact_restorable("jane@example.com");
        assert_eq!(
            redacted.restore("email removed").unwrap_err(),
            RedactionRestoreError::MissingPlaceholder
        );
        let duplicated = format!("{} {}", redacted.text(), redacted.text());
        assert_eq!(
            redacted.restore(&duplicated).unwrap_err(),
            RedactionRestoreError::DuplicatedPlaceholder
        );
    }

    #[test]
    fn restorable_redaction_debug_never_exposes_private_values() {
        let redacted = Redactor::default().redact_restorable("jane@example.com");
        let debug = format!("{redacted:?}");
        assert!(!debug.contains("jane@example.com"));
        assert!(!debug.contains(redacted.text()));
        assert!(debug.contains("[REDACTED]"));
    }
}
