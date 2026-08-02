use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use zeroize::Zeroize;

use crate::CapturePolicy;

/// Integer screen-space geometry for a normalized Accessibility element.
///
/// macOS exposes floating-point Accessibility coordinates. Providers round
/// them to integers before constructing this type so the capture model keeps
/// deterministic equality and serialization semantics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessibilityRect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl AccessibilityRect {
    fn edges(self) -> Option<(i64, i64, i64, i64)> {
        if self.width <= 0 || self.height <= 0 {
            return None;
        }
        Some((
            self.x,
            self.y,
            self.x.checked_add(self.width)?,
            self.y.checked_add(self.height)?,
        ))
    }

    fn intersects(self, other: Self) -> bool {
        let Some((left, top, right, bottom)) = self.edges() else {
            return false;
        };
        let Some((other_left, other_top, other_right, other_bottom)) = other.edges() else {
            return false;
        };
        left.max(other_left) < right.min(other_right)
            && top.max(other_top) < bottom.min(other_bottom)
    }

    fn is_above_with_horizontal_overlap(self, other: Self) -> bool {
        let Some((left, _top, right, bottom)) = self.edges() else {
            return false;
        };
        let Some((other_left, other_top, other_right, _other_bottom)) = other.edges() else {
            return false;
        };
        bottom <= other_top && left.max(other_left) < right.min(other_right)
    }

    fn contains_with_context_above(self, other: Self) -> bool {
        let Some((left, top, right, bottom)) = self.edges() else {
            return false;
        };
        let Some((other_left, other_top, other_right, other_bottom)) = other.edges() else {
            return false;
        };
        let Some(context_height) = other_top.checked_sub(top) else {
            return false;
        };
        let Some(other_height) = other_bottom.checked_sub(other_top) else {
            return false;
        };
        // A small footer or formatting-toolbar wrapper can contain the
        // composer and a sliver of space above it without containing the
        // conversation. Require enough vertical room for several composer
        // rows (and a useful absolute minimum) before treating an ancestor as
        // the context root.
        let minimum_context_height = other_height.saturating_mul(3).max(160);
        left <= other_left
            && right >= other_right
            && bottom >= other_bottom
            && context_height >= minimum_context_height
    }

    fn bottom(self) -> Option<i64> {
        self.edges().map(|(_, _, _, bottom)| bottom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum VisibleContextAlignment {
    Left,
    Center,
    Right,
}

impl VisibleContextAlignment {
    fn prefix(self) -> &'static str {
        match self {
            Self::Left => "[left] ",
            Self::Center => "[center] ",
            Self::Right => "[right] ",
        }
    }
}

struct VisibleContextCandidate {
    visual_bottom: i64,
    alignment: VisibleContextAlignment,
    text: String,
}

impl VisibleContextCandidate {
    fn zeroize_sensitive(&mut self) {
        self.text.zeroize();
    }
}

impl Drop for VisibleContextCandidate {
    fn drop(&mut self) {
        self.zeroize_sensitive();
    }
}

/// A normalized subset of an Accessibility element.
///
/// Password/secure values must never be placed in `value`; platform providers
/// mark such elements as protected and the pure pipeline refuses the capture.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessibilityNode {
    pub role: String,
    pub subrole: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<AccessibilityRect>,
    pub title: Option<String>,
    pub value: Option<String>,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
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
                let Some(mut normalized) = normalize_bounded(value, maximum_bytes) else {
                    continue;
                };
                if normalized.is_empty() {
                    normalized.zeroize();
                    continue;
                }
                if previous.as_ref() == Some(&normalized) {
                    normalized.zeroize();
                    continue;
                }
                let separator_bytes = usize::from(!output.is_empty());
                let available = maximum_bytes.saturating_sub(output.len());
                if normalized.len().saturating_add(separator_bytes) > available {
                    normalized.zeroize();
                    return;
                }
                if separator_bytes != 0 {
                    output.push('\n');
                }
                output.push_str(&normalized);
                if let Some(previous) = previous.as_mut() {
                    previous.zeroize();
                }
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
        if let Some(previous) = previous.as_mut() {
            previous.zeroize();
        }
        output
    }

    /// Returns recent visible text geometrically associated with the focused
    /// editable composer.
    ///
    /// Candidates must be non-protected, non-editable text-bearing descendants
    /// of the nearest non-window composer ancestor that contains a meaningful
    /// region above the editor. They must also be visible within the root and
    /// ancestor frames, lie wholly above the composer, and horizontally overlap
    /// it. Text is normalized, deduplicated by alignment and value, and ordered
    /// by visual bottom edge; equal geometry preserves Accessibility traversal
    /// order. Each line carries a `[left]`, `[center]`, or `[right]` geometry
    /// hint relative to the composer. The returned suffix contains at most
    /// `max_items` and `max_bytes`, including prefixes and newline separators.
    ///
    /// Missing or ambiguous composer/context geometry fails closed to `None`.
    pub fn recent_visible_context_bounded(
        &self,
        max_items: usize,
        max_bytes: usize,
    ) -> Option<String> {
        if max_items == 0 || max_bytes == 0 {
            return None;
        }
        let viewport = self.frame.filter(|frame| frame.edges().is_some())?;

        fn find_composer_path<'a>(
            node: &'a AccessibilityNode,
            path: &mut Vec<&'a AccessibilityNode>,
            found: &mut Option<Vec<&'a AccessibilityNode>>,
            ambiguous: &mut bool,
        ) {
            if *ambiguous || node.protected || is_password_role(&node.role, node.subrole.as_deref())
            {
                return;
            }
            path.push(node);
            if node.focused && is_editable_role(&node.role, node.subrole.as_deref()) {
                if found.is_some() {
                    *ambiguous = true;
                    path.pop();
                    return;
                }
                *found = Some(path.clone());
            }
            for child in &node.children {
                find_composer_path(child, path, found, ambiguous);
                if *ambiguous {
                    break;
                }
            }
            path.pop();
        }

        let mut path = Vec::new();
        let mut composer_path = None;
        let mut ambiguous = false;
        find_composer_path(self, &mut path, &mut composer_path, &mut ambiguous);
        if ambiguous {
            return None;
        }
        let composer_path = composer_path?;
        let composer = *composer_path.last()?;
        let composer = composer.frame.filter(|frame| frame.edges().is_some())?;
        if !composer.intersects(viewport) {
            return None;
        }
        let ancestor_end = composer_path.len().checked_sub(1)?;
        let context_root = composer_path
            .get(1..ancestor_end)?
            .iter()
            .rev()
            .copied()
            .find(|ancestor| {
                !ancestor.role.eq_ignore_ascii_case("AXWindow")
                    && !ancestor.protected
                    && !is_password_role(&ancestor.role, ancestor.subrole.as_deref())
                    && !is_editable_role(&ancestor.role, ancestor.subrole.as_deref())
                    && ancestor
                        .frame
                        .is_some_and(|frame| frame.contains_with_context_above(composer))
            })?;
        let context_viewport = context_root.frame?;

        fn collect(
            node: &AccessibilityNode,
            viewport: AccessibilityRect,
            context_viewport: AccessibilityRect,
            composer: AccessibilityRect,
            max_bytes: usize,
            candidates: &mut Vec<VisibleContextCandidate>,
        ) {
            if node.protected
                || is_password_role(&node.role, node.subrole.as_deref())
                || is_editable_role(&node.role, node.subrole.as_deref())
            {
                return;
            }

            if let Some(frame) = node.frame.filter(|frame| {
                frame.intersects(viewport)
                    && frame.intersects(context_viewport)
                    && frame.is_above_with_horizontal_overlap(composer)
            }) {
                if let Some(visual_bottom) = frame.bottom() {
                    for value in [
                        node.title.as_deref(),
                        node.value.as_deref(),
                        node.description.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if let Some(mut normalized) = normalize_bounded(value, max_bytes) {
                            if normalized.is_empty() {
                                normalized.zeroize();
                            } else {
                                candidates.push(VisibleContextCandidate {
                                    visual_bottom,
                                    alignment: visible_context_alignment(frame, composer),
                                    text: normalized,
                                });
                            }
                        }
                    }
                }
            }

            for child in &node.children {
                collect(
                    child,
                    viewport,
                    context_viewport,
                    composer,
                    max_bytes,
                    candidates,
                );
            }
        }

        let mut candidates = Vec::new();
        collect(
            context_root,
            viewport,
            context_viewport,
            composer,
            max_bytes,
            &mut candidates,
        );
        // `sort_by_key` is stable, so fields at the same visual position retain
        // the provider's traversal order.
        candidates.sort_by_key(|candidate| candidate.visual_bottom);

        let (mut selected, selected_bytes) = {
            // Walk bottom-to-top so duplicate text retains its visually newest
            // occurrence. Store indexes rather than extra unredacted strings.
            let mut seen = HashSet::new();
            let mut selected = Vec::new();
            let mut selected_bytes = 0_usize;
            for (index, candidate) in candidates.iter().enumerate().rev() {
                if !seen.insert((candidate.alignment, candidate.text.as_str())) {
                    continue;
                }
                if selected.len() >= max_items {
                    break;
                }
                let separator_bytes = usize::from(!selected.is_empty());
                let Some(next_bytes) = selected_bytes
                    .checked_add(separator_bytes)
                    .and_then(|bytes| bytes.checked_add(candidate.alignment.prefix().len()))
                    .and_then(|bytes| bytes.checked_add(candidate.text.len()))
                else {
                    break;
                };
                if next_bytes > max_bytes {
                    break;
                }
                selected_bytes = next_bytes;
                selected.push(index);
            }
            (selected, selected_bytes)
        };
        selected.reverse();
        let result = if selected.is_empty() {
            None
        } else {
            let mut output = String::with_capacity(selected_bytes);
            for index in selected {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(candidates[index].alignment.prefix());
                output.push_str(&candidates[index].text);
            }
            Some(output)
        };
        zeroize_visible_context_candidates(&mut candidates);
        result
    }

    /// Wipes every owned Accessibility string before releasing its storage.
    pub fn zeroize_sensitive(&mut self) {
        self.role.zeroize();
        zeroize_optional(&mut self.subrole);
        zeroize_optional(&mut self.title);
        zeroize_optional(&mut self.value);
        zeroize_optional(&mut self.description);
        zeroize_optional(&mut self.placeholder);
        zeroize_optional(&mut self.identifier);
        zeroize_optional(&mut self.url);
        for child in &mut self.children {
            child.zeroize_sensitive();
        }
        self.children.clear();
    }
}

fn visible_context_alignment(
    candidate: AccessibilityRect,
    composer: AccessibilityRect,
) -> VisibleContextAlignment {
    // Compare doubled centers in i128 space so untrusted AX coordinates cannot
    // overflow. A narrow central dead zone avoids assigning authorship to
    // full-width labels or centered chrome from sub-pixel layout jitter.
    let candidate_center = i128::from(candidate.x)
        .saturating_mul(2)
        .saturating_add(i128::from(candidate.width));
    let composer_center = i128::from(composer.x)
        .saturating_mul(2)
        .saturating_add(i128::from(composer.width));
    let dead_zone = i128::from(composer.width.max(0)) / 5;
    let delta = candidate_center.saturating_sub(composer_center);
    if delta < -dead_zone {
        VisibleContextAlignment::Left
    } else if delta > dead_zone {
        VisibleContextAlignment::Right
    } else {
        VisibleContextAlignment::Center
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
            normalized.zeroize();
            return None;
        }
        if separator_bytes != 0 {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    Some(normalized)
}

fn zeroize_visible_context_candidates(candidates: &mut [VisibleContextCandidate]) {
    for candidate in candidates {
        candidate.zeroize_sensitive();
    }
}

fn is_password_role(role: &str, subrole: Option<&str>) -> bool {
    let combined = format!("{role} {}", subrole.unwrap_or_default()).to_ascii_lowercase();
    combined.contains("securetextfield")
        || combined.contains("password")
        || combined.contains("secure text")
}

fn is_editable_role(role: &str, subrole: Option<&str>) -> bool {
    matches!(
        role.to_ascii_lowercase().as_str(),
        "axtextarea" | "axtextfield" | "axsearchfield" | "axcombobox"
    ) || subrole.is_some_and(|subrole| subrole.eq_ignore_ascii_case("AXTextEntryArea"))
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
    pub window_id: Option<i64>,
    pub browser_url: Option<String>,
}

impl CaptureMetadata {
    pub fn zeroize_sensitive(&mut self) {
        self.app_name.zeroize();
        zeroize_optional(&mut self.bundle_id);
        zeroize_optional(&mut self.window_title);
        self.window_id = None;
        zeroize_optional(&mut self.browser_url);
    }

    pub fn into_raw_capture(mut self, root: AccessibilityNode) -> RawCapture {
        RawCapture {
            captured_at_ms: self.captured_at_ms,
            pid: self.pid,
            app_name: std::mem::take(&mut self.app_name),
            bundle_id: self.bundle_id.take(),
            window_title: self.window_title.take(),
            window_id: self.window_id.take(),
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

/// Runs a contextual-reply tree reader only for the two explicitly supported
/// surfaces authorized by shallow metadata and the URL-only AX observation.
/// Browser capture requires every observed URL to identify WhatsApp; duplicate
/// observations of the address control and active web area are allowed.
pub fn capture_contextual_reply_after_surface_preflight<T>(
    bundle_id: Option<&str>,
    browser_urls: &[String],
    read_full_tree: impl FnOnce() -> Result<T, CaptureError>,
) -> Result<T, CaptureError> {
    let slack = bundle_id == Some("com.tinyspeck.slackmacgap");
    let whatsapp = !browser_urls.is_empty()
        && browser_urls.iter().all(|value| {
            Url::parse(value).ok().is_some_and(|url| {
                url.scheme() == "https"
                    && url.host_str() == Some("web.whatsapp.com")
                    && url.port_or_known_default() == Some(443)
            })
        });
    if !slack && !whatsapp {
        return Err(CaptureError::UnsupportedSurface);
    }
    read_full_tree()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCapture {
    pub captured_at_ms: i64,
    pub pid: i32,
    pub app_name: String,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<i64>,
    pub browser_url: Option<String>,
    pub secure_input: bool,
    pub root: AccessibilityNode,
}

impl RawCapture {
    pub fn zeroize_sensitive(&mut self) {
        self.app_name.zeroize();
        zeroize_optional(&mut self.bundle_id);
        zeroize_optional(&mut self.window_title);
        self.window_id = None;
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

    /// Captures only when the current shallow foreground metadata still
    /// identifies the caller's original target.
    ///
    /// Platform providers must override this and perform the comparison before
    /// recursively reading Accessibility text. The default performs no
    /// capture, so adding a provider cannot silently weaken this boundary.
    async fn capture_foreground_for_target(
        &self,
        _policy: &CapturePolicy,
        _expected_pid: i32,
        _expected_window_title: &str,
        _expected_window_id: Option<i64>,
    ) -> Result<ForegroundCapture, CaptureError> {
        Err(CaptureError::Accessibility(
            "targeted Accessibility capture is unavailable".to_owned(),
        ))
    }
}

/// Validates exact foreground identity from shallow Accessibility metadata.
/// This helper intentionally compares the complete window title rather than a
/// prefix or substring so similarly named windows cannot authorize a read.
pub fn validate_capture_target(
    actual_pid: i32,
    actual_window_title: Option<&str>,
    actual_window_id: Option<i64>,
    expected_pid: i32,
    expected_window_title: &str,
    expected_window_id: Option<i64>,
) -> Result<(), CaptureError> {
    if expected_pid <= 0
        || expected_window_title.trim().is_empty()
        || expected_window_title.trim() != expected_window_title
        || expected_window_id.is_some_and(|window_id| window_id <= 0)
        || actual_pid != expected_pid
        || actual_window_title != Some(expected_window_title)
        || expected_window_id.is_some_and(|window_id| actual_window_id != Some(window_id))
    {
        return Err(CaptureError::TargetMismatch);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("Accessibility permission has not been granted")]
    PermissionDenied,
    #[error("secure keyboard input is active")]
    SecureInput,
    #[error("no focused application is available")]
    NoFocusedApplication,
    #[error("the foreground application no longer matches the requested target")]
    TargetMismatch,
    #[error("the foreground target is not a supported contextual-reply surface")]
    UnsupportedSurface,
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
            window_id: Some(9_001),
            browser_url: Some(["https", "://", "secret.example.com/report"].concat()),
        }
    }

    fn rect(x: i64, y: i64, width: i64, height: i64) -> AccessibilityRect {
        AccessibilityRect {
            x,
            y,
            width,
            height,
        }
    }

    fn visible_text(value: &str, frame: AccessibilityRect) -> AccessibilityNode {
        AccessibilityNode {
            role: "AXStaticText".to_owned(),
            frame: Some(frame),
            value: Some(value.to_owned()),
            ..AccessibilityNode::default()
        }
    }

    fn composer(frame: Option<AccessibilityRect>) -> AccessibilityNode {
        AccessibilityNode {
            role: "AXTextArea".to_owned(),
            frame,
            value: Some("unfinished draft".to_owned()),
            focused: true,
            ..AccessibilityNode::default()
        }
    }

    fn context_root(children: Vec<AccessibilityNode>) -> AccessibilityNode {
        AccessibilityNode {
            role: "AXWindow".to_owned(),
            frame: Some(rect(0, 0, 1_000, 800)),
            children: vec![AccessibilityNode {
                role: "AXGroup".to_owned(),
                frame: Some(rect(300, 120, 650, 630)),
                children,
                ..AccessibilityNode::default()
            }],
            ..AccessibilityNode::default()
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
        assert!(metadata.window_id.is_none());
        assert!(metadata.browser_url.is_none());
    }

    #[test]
    fn unsupported_contextual_reply_surface_never_calls_the_tree_reader() {
        let reads = Cell::new(0);
        let unsupported_urls = vec!["https://example.com/chat".to_owned()];
        let result = capture_contextual_reply_after_surface_preflight(
            Some("com.google.Chrome"),
            &unsupported_urls,
            || {
                reads.set(reads.get() + 1);
                Ok(AccessibilityNode::default())
            },
        );
        assert!(matches!(result, Err(CaptureError::UnsupportedSurface)));
        assert_eq!(reads.get(), 0);

        let whatsapp_urls = vec![
            "https://web.whatsapp.com/chat".to_owned(),
            "https://web.whatsapp.com/chat".to_owned(),
        ];
        capture_contextual_reply_after_surface_preflight(
            Some("com.google.Chrome"),
            &whatsapp_urls,
            || {
                reads.set(reads.get() + 1);
                Ok(AccessibilityNode::default())
            },
        )
        .expect("exact WhatsApp host");
        let mixed_urls = vec![
            "https://web.whatsapp.com/chat".to_owned(),
            "https://example.com/chat".to_owned(),
        ];
        let mixed = capture_contextual_reply_after_surface_preflight(
            Some("com.google.Chrome"),
            &mixed_urls,
            || {
                reads.set(reads.get() + 1);
                Ok(AccessibilityNode::default())
            },
        );
        assert!(matches!(mixed, Err(CaptureError::UnsupportedSurface)));
        capture_contextual_reply_after_surface_preflight(
            Some("com.tinyspeck.slackmacgap"),
            &[],
            || {
                reads.set(reads.get() + 1);
                Ok(AccessibilityNode::default())
            },
        )
        .expect("exact Slack bundle");
        assert_eq!(reads.get(), 2);
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
                placeholder: Some("private placeholder".to_owned()),
                ..AccessibilityNode::default()
            }],
            ..AccessibilityNode::default()
        });
        capture.zeroize_sensitive();
        assert!(capture.app_name.is_empty());
        assert!(capture.bundle_id.is_none());
        assert!(capture.window_title.is_none());
        assert!(capture.window_id.is_none());
        assert!(capture.browser_url.is_none());
        assert!(capture.root.role.is_empty());
        assert!(capture.root.value.is_none());
        assert!(capture.root.children.is_empty());
    }

    #[test]
    fn exact_target_preflight_rejects_mismatches_and_blank_expectations() {
        assert!(validate_capture_target(
            42,
            Some("Roadmap — Slack"),
            Some(9_001),
            42,
            "Roadmap — Slack",
            Some(9_001),
        )
        .is_ok());
        assert!(matches!(
            validate_capture_target(
                7,
                Some("Roadmap — Slack"),
                Some(9_001),
                42,
                "Roadmap — Slack",
                Some(9_001),
            ),
            Err(CaptureError::TargetMismatch)
        ));
        assert!(matches!(
            validate_capture_target(
                42,
                Some("Other"),
                Some(9_001),
                42,
                "Roadmap — Slack",
                Some(9_001),
            ),
            Err(CaptureError::TargetMismatch)
        ));
        assert!(matches!(
            validate_capture_target(
                42,
                Some("Roadmap — Slack"),
                Some(9_001),
                42,
                " ",
                Some(9_001),
            ),
            Err(CaptureError::TargetMismatch)
        ));
        assert!(matches!(
            validate_capture_target(
                42,
                Some("Roadmap — Slack"),
                Some(9_002),
                42,
                "Roadmap — Slack",
                Some(9_001),
            ),
            Err(CaptureError::TargetMismatch)
        ));
        assert!(validate_capture_target(
            42,
            Some("Roadmap — Slack"),
            None,
            42,
            "Roadmap — Slack",
            None,
        )
        .is_ok());
        assert!(validate_capture_target(
            42,
            Some("Roadmap — Slack"),
            Some(9_001),
            42,
            "Roadmap — Slack",
            None,
        )
        .is_ok());
    }

    #[test]
    fn recent_context_excludes_sidebar_composer_and_non_visible_geometry() {
        let root = context_root(vec![
            visible_text("sidebar contact", rect(20, 120, 250, 30)),
            visible_text("first visible message", rect(340, 560, 420, 40)),
            visible_text("second visible message", rect(420, 630, 480, 40)),
            // Touching the composer's left edge is not horizontal overlap.
            visible_text("adjacent sidebar", rect(100, 200, 200, 40)),
            // This node overlaps the composer vertically and is not above it.
            visible_text("below message viewport", rect(340, 690, 400, 30)),
            // Entirely outside the root viewport.
            visible_text("offscreen message", rect(340, -80, 400, 30)),
            composer(Some(rect(300, 700, 650, 50))),
        ]);

        assert_eq!(
            root.recent_visible_context_bounded(20, 8 * 1024),
            Some("[left] first visible message\n[center] second visible message".to_owned())
        );
    }

    #[test]
    fn recent_context_skips_protected_password_and_editable_subtrees() {
        let protected = AccessibilityNode {
            role: "AXGroup".to_owned(),
            frame: Some(rect(340, 520, 400, 50)),
            value: Some("protected parent".to_owned()),
            protected: true,
            children: vec![visible_text("protected child", rect(350, 530, 380, 30))],
            ..AccessibilityNode::default()
        };
        let password = AccessibilityNode {
            role: "AXSecureTextField".to_owned(),
            frame: Some(rect(340, 580, 400, 40)),
            value: Some("password value".to_owned()),
            children: vec![visible_text("password child", rect(350, 585, 380, 20))],
            ..AccessibilityNode::default()
        };
        let other_editor = AccessibilityNode {
            role: "AXTextField".to_owned(),
            frame: Some(rect(340, 630, 400, 40)),
            value: Some("other editable value".to_owned()),
            children: vec![visible_text("editable child", rect(350, 635, 380, 20))],
            ..AccessibilityNode::default()
        };
        let root = context_root(vec![
            visible_text("safe message", rect(340, 460, 400, 40)),
            protected,
            password,
            other_editor,
            composer(Some(rect(300, 700, 650, 50))),
        ]);

        assert_eq!(
            root.recent_visible_context_bounded(20, 8 * 1024),
            Some("[left] safe message".to_owned())
        );
    }

    #[test]
    fn recent_context_orders_out_of_order_nodes_by_visual_recency() {
        let root = context_root(vec![
            // AX traversal order deliberately does not match screen order.
            visible_text("gamma", rect(340, 570, 400, 30)),
            visible_text("alpha", rect(340, 420, 400, 30)),
            visible_text("beta", rect(340, 470, 400, 30)),
            visible_text("alpha", rect(340, 520, 400, 30)),
            composer(Some(rect(300, 700, 650, 50))),
        ]);

        assert_eq!(
            root.recent_visible_context_bounded(3, 8 * 1024),
            Some("[left] beta\n[left] alpha\n[left] gamma".to_owned())
        );
        assert_eq!(
            root.recent_visible_context_bounded(2, 8 * 1024),
            Some("[left] alpha\n[left] gamma".to_owned())
        );
    }

    #[test]
    fn recent_context_preserves_traversal_order_for_equal_geometry() {
        let shared_frame = rect(340, 560, 400, 30);
        let root = context_root(vec![
            visible_text("first field", shared_frame),
            visible_text("second field", shared_frame),
            visible_text("newest message", rect(340, 620, 400, 30)),
            composer(Some(rect(300, 700, 650, 50))),
        ]);

        assert_eq!(
            root.recent_visible_context_bounded(20, 8 * 1024),
            Some("[left] first field\n[left] second field\n[left] newest message".to_owned())
        );
    }

    #[test]
    fn recent_context_is_rooted_at_composer_container_not_window() {
        let conversation = AccessibilityNode {
            role: "AXGroup".to_owned(),
            frame: Some(rect(300, 160, 650, 590)),
            children: vec![
                visible_text("only recent message", rect(340, 620, 400, 30)),
                composer(Some(rect(300, 700, 650, 50))),
            ],
            ..AccessibilityNode::default()
        };
        let root = AccessibilityNode {
            role: "AXWindow".to_owned(),
            frame: Some(rect(0, 0, 1_000, 800)),
            children: vec![
                visible_text("browser toolbar", rect(0, 20, 1_000, 40)),
                visible_text("chat header", rect(0, 100, 1_000, 40)),
                visible_text("sidebar contact", rect(20, 500, 250, 30)),
                conversation,
            ],
            ..AccessibilityNode::default()
        };

        assert_eq!(
            root.recent_visible_context_bounded(20, 8 * 1024),
            Some("[left] only recent message".to_owned())
        );
    }

    #[test]
    fn recent_context_skips_a_nested_footer_wrapper_as_the_context_root() {
        let footer = AccessibilityNode {
            role: "AXGroup".to_owned(),
            frame: Some(rect(300, 650, 650, 100)),
            children: vec![composer(Some(rect(300, 700, 650, 50)))],
            ..AccessibilityNode::default()
        };
        let root = AccessibilityNode {
            role: "AXWindow".to_owned(),
            frame: Some(rect(0, 0, 1_000, 800)),
            children: vec![AccessibilityNode {
                role: "AXGroup".to_owned(),
                frame: Some(rect(300, 100, 650, 650)),
                children: vec![
                    visible_text("conversation message", rect(340, 590, 400, 30)),
                    footer,
                ],
                ..AccessibilityNode::default()
            }],
            ..AccessibilityNode::default()
        };

        assert_eq!(
            root.recent_visible_context_bounded(20, 8 * 1024),
            Some("[left] conversation message".to_owned())
        );
    }

    #[test]
    fn recent_context_preserves_visual_order_and_message_alignment() {
        let root = context_root(vec![
            // AX traversal order is deliberately newest-first and does not
            // match the screen. Identical text on opposite sides remains
            // distinct because alignment carries authorship evidence.
            visible_text("Sounds good", rect(650, 600, 250, 32)),
            visible_text("Today", rect(525, 400, 200, 24)),
            visible_text("Sounds good", rect(330, 510, 250, 32)),
            composer(Some(rect(300, 700, 650, 50))),
        ]);

        assert_eq!(
            root.recent_visible_context_bounded(20, 8 * 1024),
            Some("[center] Today\n[left] Sounds good\n[right] Sounds good".to_owned())
        );
    }

    #[test]
    fn recent_context_enforces_utf8_byte_bounds_without_fragmenting_items() {
        let root = context_root(vec![
            // Deliberately newer-first in AX traversal order.
            visible_text("犬", rect(340, 610, 400, 30)),
            visible_text("é", rect(340, 560, 400, 30)),
            composer(Some(rect(300, 700, 650, 50))),
        ]);

        let exact = root
            .recent_visible_context_bounded(20, 20)
            .expect("two complete UTF-8 items fit exactly");
        assert_eq!(exact, "[left] é\n[left] 犬");
        assert_eq!(exact.len(), 20);
        assert_eq!(
            root.recent_visible_context_bounded(20, 10),
            Some("[left] 犬".to_owned())
        );

        let multibyte_only = context_root(vec![
            visible_text("🐕", rect(340, 610, 400, 30)),
            composer(Some(rect(300, 700, 650, 50))),
        ]);
        assert_eq!(
            multibyte_only.recent_visible_context_bounded(20, 11),
            Some("[left] 🐕".to_owned())
        );
        assert_eq!(multibyte_only.recent_visible_context_bounded(20, 10), None);
    }

    #[test]
    fn recent_context_wipes_owned_candidate_buffers() {
        let mut candidates = vec![
            VisibleContextCandidate {
                visual_bottom: 10,
                alignment: VisibleContextAlignment::Left,
                text: "private candidate".to_owned(),
            },
            VisibleContextCandidate {
                visual_bottom: 20,
                alignment: VisibleContextAlignment::Right,
                text: "another private candidate".to_owned(),
            },
        ];

        zeroize_visible_context_candidates(&mut candidates);

        assert!(candidates.iter().all(|candidate| candidate.text.is_empty()));
    }

    #[test]
    fn recent_context_fails_closed_without_unambiguous_geometry_and_content() {
        let message = visible_text("message", rect(340, 600, 400, 30));

        let mut no_viewport = context_root(vec![
            message.clone(),
            composer(Some(rect(300, 700, 650, 50))),
        ]);
        no_viewport.frame = None;
        assert_eq!(no_viewport.recent_visible_context_bounded(20, 1_024), None);

        let no_composer_frame = context_root(vec![message.clone(), composer(None)]);
        assert_eq!(
            no_composer_frame.recent_visible_context_bounded(20, 1_024),
            None
        );

        let multiple_composers = context_root(vec![
            message,
            composer(Some(rect(300, 700, 650, 50))),
            composer(Some(rect(300, 640, 650, 40))),
        ]);
        assert_eq!(
            multiple_composers.recent_visible_context_bounded(20, 1_024),
            None
        );

        let no_context = context_root(vec![composer(Some(rect(300, 700, 650, 50)))]);
        assert_eq!(no_context.recent_visible_context_bounded(20, 1_024), None);
        assert_eq!(no_context.recent_visible_context_bounded(0, 1_024), None);
        assert_eq!(no_context.recent_visible_context_bounded(20, 0), None);

        let window_is_not_an_eligible_context_container = AccessibilityNode {
            role: "AXWindow".to_owned(),
            frame: Some(rect(0, 0, 1_000, 800)),
            children: vec![
                visible_text("window-level text", rect(340, 600, 400, 30)),
                composer(Some(rect(300, 700, 650, 50))),
            ],
            ..AccessibilityNode::default()
        };
        assert_eq!(
            window_is_not_an_eligible_context_container.recent_visible_context_bounded(20, 1_024),
            None
        );
    }
}
