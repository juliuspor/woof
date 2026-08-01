use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroize;

use crate::CapturePolicy;

/// A normalized subset of an Accessibility element.
///
/// Password/secure values must never be placed in `value`; platform providers
/// mark such elements as protected and the pure pipeline refuses the capture.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessibilityNode {
    pub role: String,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    pub identifier: Option<String>,
    pub url: Option<String>,
    pub focused: bool,
    pub protected: bool,
    pub children: Vec<AccessibilityNode>,
}

impl AccessibilityNode {
    pub fn has_protected_content(&self) -> bool {
        self.protected
            || is_password_role(&self.role, self.subrole.as_deref())
            || self.children.iter().any(Self::has_protected_content)
    }

    /// Returns role/title breadcrumbs from the root to the focused element.
    pub fn focused_breadcrumbs(&self) -> Vec<String> {
        fn visit(node: &AccessibilityNode, path: &mut Vec<String>) -> Option<Vec<String>> {
            let label = node
                .title
                .as_deref()
                .filter(|title| !title.trim().is_empty())
                .unwrap_or(node.role.as_str());
            path.push(label.to_owned());
            if node.focused {
                return Some(path.clone());
            }
            for child in &node.children {
                if let Some(found) = visit(child, path) {
                    return Some(found);
                }
            }
            path.pop();
            None
        }

        visit(self, &mut Vec::new()).unwrap_or_default()
    }

    /// Returns the Accessibility role of the focused element, if one exists.
    ///
    /// Platform providers normalize the native AX role into `role`; keeping it
    /// separate from the display-oriented breadcrumb preserves values such as
    /// `AXTextArea` even when the focused element has a human-readable title.
    pub fn focused_role(&self) -> Option<&str> {
        if self.focused {
            let role = self.role.trim();
            return (!role.is_empty()).then_some(role);
        }
        self.children.iter().find_map(Self::focused_role)
    }

    /// Collects visible text into a fixed-size buffer while ignoring adjacent
    /// duplicates. An individual field that cannot fit is skipped whole so a
    /// private token is never cut into a fragment before redaction.
    pub fn visible_text_bounded(&self, maximum_bytes: usize) -> String {
        fn visit(
            node: &AccessibilityNode,
            maximum_bytes: usize,
            output: &mut String,
            previous: &mut Option<String>,
        ) {
            if output.len() >= maximum_bytes
                || node.protected
                || is_password_role(&node.role, node.subrole.as_deref())
            {
                return;
            }
            for value in [
                node.title.as_deref(),
                node.value.as_deref(),
                node.description.as_deref(),
            ]
            .into_iter()
            .flatten()
            {
                let Some(normalized) = normalize_bounded(value, maximum_bytes) else {
                    continue;
                };
                if normalized.is_empty() || previous.as_ref() == Some(&normalized) {
                    continue;
                }
                let separator_bytes = usize::from(!output.is_empty());
                let available = maximum_bytes.saturating_sub(output.len());
                if normalized.len().saturating_add(separator_bytes) > available {
                    return;
                }
                if separator_bytes != 0 {
                    output.push('\n');
                }
                output.push_str(&normalized);
                *previous = Some(normalized);
            }
            for child in &node.children {
                visit(child, maximum_bytes, output, previous);
                if output.len() >= maximum_bytes {
                    return;
                }
            }
        }

        let mut output = String::with_capacity(maximum_bytes.min(4 * 1024));
        let mut previous = None;
        visit(self, maximum_bytes, &mut output, &mut previous);
        output
    }

    /// Wipes every owned Accessibility string before releasing its storage.
    pub fn zeroize_sensitive(&mut self) {
        self.role.zeroize();
        zeroize_optional(&mut self.subrole);
        zeroize_optional(&mut self.title);
        zeroize_optional(&mut self.value);
        zeroize_optional(&mut self.description);
        zeroize_optional(&mut self.identifier);
        zeroize_optional(&mut self.url);
        for child in &mut self.children {
            child.zeroize_sensitive();
        }
        self.children.clear();
    }
}

fn normalize_bounded(value: &str, maximum_bytes: usize) -> Option<String> {
    let mut normalized = String::with_capacity(value.len().min(maximum_bytes));
    for word in value.split_whitespace() {
        let separator_bytes = usize::from(!normalized.is_empty());
        if word
            .len()
            .saturating_add(separator_bytes)
            .saturating_add(normalized.len())
            > maximum_bytes
        {
            return None;
        }
        if separator_bytes != 0 {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    Some(normalized)
}

fn is_password_role(role: &str, subrole: Option<&str>) -> bool {
    let combined = format!("{role} {}", subrole.unwrap_or_default()).to_ascii_lowercase();
    combined.contains("securetextfield")
        || combined.contains("password")
        || combined.contains("secure text")
}

fn zeroize_optional(value: &mut Option<String>) {
    if let Some(value) = value {
        value.zeroize();
    }
    *value = None;
}

/// Foreground fields that can be obtained without recursively reading the
/// window's Accessibility text tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureMetadata {
    pub captured_at_ms: i64,
    pub pid: i32,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub browser_url: Option<String>,
}

impl CaptureMetadata {
    pub fn zeroize_sensitive(&mut self) {
        self.app_name.zeroize();
        zeroize_optional(&mut self.bundle_id);
        zeroize_optional(&mut self.window_title);
        zeroize_optional(&mut self.browser_url);
    }

    pub fn into_raw_capture(mut self, root: AccessibilityNode) -> RawCapture {
        RawCapture {
            captured_at_ms: self.captured_at_ms,
            pid: self.pid,
            app_name: std::mem::take(&mut self.app_name),
            bundle_id: self.bundle_id.take(),
            window_title: self.window_title.take(),
            browser_url: self.browser_url.take(),
            secure_input: false,
            root,
        }
    }
}

impl Drop for CaptureMetadata {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

/// Runs a full-tree reader only after metadata has passed the capture policy.
/// Rejected metadata and failed reads are wiped before returning.
pub fn capture_after_preflight<T>(
    mut metadata: CaptureMetadata,
    policy: &CapturePolicy,
    read_full_tree: impl FnOnce() -> Result<T, CaptureError>,
) -> Result<Option<(CaptureMetadata, T)>, CaptureError> {
    if policy.is_blacklisted_metadata(&metadata) {
        metadata.zeroize_sensitive();
        return Ok(None);
    }
    let captured = read_full_tree()?;
    Ok(Some((metadata, captured)))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCapture {
    pub captured_at_ms: i64,
    pub pid: i32,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub browser_url: Option<String>,
    pub secure_input: bool,
    pub root: AccessibilityNode,
}

impl RawCapture {
    pub fn zeroize_sensitive(&mut self) {
        self.app_name.zeroize();
        zeroize_optional(&mut self.bundle_id);
        zeroize_optional(&mut self.window_title);
        zeroize_optional(&mut self.browser_url);
        self.root.zeroize_sensitive();
    }
}

impl Drop for RawCapture {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

pub enum ForegroundCapture {
    Captured(Box<RawCapture>),
    Blacklisted,
}

#[async_trait]
pub trait AccessibilityProvider: Send + Sync {
    async fn capture_foreground(
        &self,
        policy: &CapturePolicy,
    ) -> Result<ForegroundCapture, CaptureError>;
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("Accessibility permission has not been granted")]
    PermissionDenied,
    #[error("secure keyboard input is active")]
    SecureInput,
    #[error("no focused application is available")]
    NoFocusedApplication,
    #[error("Accessibility operation failed: {0}")]
    Accessibility(String),
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use crate::{BlacklistKind, BlacklistRule};

    use super::*;

    fn private_metadata() -> CaptureMetadata {
        CaptureMetadata {
            captured_at_ms: 1,
            pid: 42,
            app_name: "Private Browser".to_owned(),
            bundle_id: Some("com.example.private".to_owned()),
            window_title: Some("Confidential payroll".to_owned()),
            browser_url: Some(["https", "://", "secret.example.com/report"].concat()),
        }
    }

    #[test]
    fn rejected_preflight_wipes_metadata_without_calling_tree_reader() {
        let policy = CapturePolicy::new([BlacklistRule {
            kind: BlacklistKind::BrowserHost,
            pattern: "example.com".to_owned(),
        }]);
        let reads = Cell::new(0);
        let result = capture_after_preflight(private_metadata(), &policy, || {
            reads.set(reads.get() + 1);
            Ok(AccessibilityNode::default())
        })
        .expect("preflight");
        assert!(result.is_none());
        assert_eq!(reads.get(), 0);

        let mut metadata = private_metadata();
        metadata.zeroize_sensitive();
        assert!(metadata.app_name.is_empty());
        assert!(metadata.bundle_id.is_none());
        assert!(metadata.window_title.is_none());
        assert!(metadata.browser_url.is_none());
    }

    #[test]
    fn raw_capture_wipe_clears_metadata_and_recursive_text() {
        let metadata = private_metadata();
        let mut capture = metadata.into_raw_capture(AccessibilityNode {
            role: "AXWindow".to_owned(),
            value: Some("private root text".to_owned()),
            children: vec![AccessibilityNode {
                role: "AXTextArea".to_owned(),
                title: Some("private child title".to_owned()),
                ..AccessibilityNode::default()
            }],
            ..AccessibilityNode::default()
        });
        capture.zeroize_sensitive();
        assert!(capture.app_name.is_empty());
        assert!(capture.bundle_id.is_none());
        assert!(capture.window_title.is_none());
        assert!(capture.browser_url.is_none());
        assert!(capture.root.role.is_empty());
        assert!(capture.root.value.is_none());
        assert!(capture.root.children.is_empty());
    }
}
