use crate::{
    with_temporary_text, Clipboard, DeliveryFocus, DeliveryMethod, FallbackTarget,
    FocusedElementMetadata, InlineError, InlineRead, PreviewWriteError, ReplacementAttempt,
    TextScope, WakeHint,
};

pub trait FocusedTextTarget: Send {
    fn metadata(&self) -> Result<FocusedElementMetadata, InlineError>;
    fn read(&self, scope: TextScope) -> Result<InlineRead, InlineError>;
    fn validate(&self, expected: &InlineRead, focus: DeliveryFocus) -> Result<(), InlineError>;
    fn validate_controller_focus(&self, controller_pid: i32) -> Result<(), InlineError>;
    fn replace(
        &mut self,
        expected: &InlineRead,
        replacement: &str,
        focus: DeliveryFocus,
    ) -> Result<ReplacementAttempt, InlineError>;
    /// Replace a whole draft through Accessibility only and return the exact
    /// observed revision. This is used for short-lived, session-owned reply
    /// progress text and deliberately has no clipboard or keyboard fallback.
    fn replace_whole_draft_preview(
        &mut self,
        expected_text: &str,
        replacement: &str,
        controller_pid: i32,
    ) -> Result<InlineRead, PreviewWriteError>;
    /// Restore a session-owned preview only when the retained element still
    /// contains that exact revision. Cleanup never claims unrelated focus and
    /// never overwrites user edits.
    fn restore_whole_draft_preview(
        &mut self,
        expected_previews: &[&str],
        original: &InlineRead,
        controller_pid: i32,
    ) -> Result<InlineRead, InlineError>;
    fn prepare_fallback(
        &mut self,
        expected: &InlineRead,
        hint: WakeHint,
        focus: DeliveryFocus,
    ) -> Result<FallbackTarget, InlineError>;
    fn validate_fallback(
        &self,
        expected: &InlineRead,
        fallback: FallbackTarget,
    ) -> Result<(), InlineError>;
    fn confirm_whole_draft(&self, replacement: &str) -> Result<(), InlineError>;
    fn release(&mut self);
}

/// Insertion-only keyboard fallback. This interface intentionally exposes no
/// Return, Enter, or submit action.
pub trait InputInjector: Send {
    fn paste(&mut self, pid: i32) -> Result<(), InlineError>;
    fn type_unicode(&mut self, pid: i32, value: &str) -> Result<(), InlineError>;
}

pub struct InlineSession<T, C, I>
where
    T: FocusedTextTarget,
    C: Clipboard,
    I: InputInjector,
{
    target: T,
    clipboard: C,
    input: I,
    released: bool,
}

impl<T, C, I> InlineSession<T, C, I>
where
    T: FocusedTextTarget,
    C: Clipboard,
    I: InputInjector,
{
    pub fn new(target: T, clipboard: C, input: I) -> Self {
        Self {
            target,
            clipboard,
            input,
            released: false,
        }
    }

    pub fn metadata(&self) -> Result<FocusedElementMetadata, InlineError> {
        self.ensure_active()?;
        self.target.metadata()
    }

    pub fn read(&self, scope: TextScope) -> Result<InlineRead, InlineError> {
        self.ensure_active()?;
        self.target.read(scope)
    }

    pub fn validate(&self, expected: &InlineRead, focus: DeliveryFocus) -> Result<(), InlineError> {
        self.ensure_active()?;
        self.target.validate(expected, focus)
    }

    pub fn validate_controller_focus(&self, controller_pid: i32) -> Result<(), InlineError> {
        self.ensure_active()?;
        self.target.validate_controller_focus(controller_pid)
    }

    pub fn deliver(
        &mut self,
        expected: &InlineRead,
        replacement: &str,
        wake_hint: WakeHint,
        focus: DeliveryFocus,
    ) -> Result<DeliveryMethod, InlineError> {
        self.ensure_active()?;
        self.target.validate(expected, focus)?;
        if let Some(method) =
            Option::<DeliveryMethod>::from(self.target.replace(expected, replacement, focus)?)
        {
            return self.confirm_delivery(expected, replacement, method);
        }

        let fallback = self.target.prepare_fallback(expected, wake_hint, focus)?;
        self.target.validate_fallback(expected, fallback)?;
        if replacement.is_empty() {
            self.target.validate_fallback(expected, fallback)?;
            self.input.type_unicode(fallback.pid, replacement)?;
            return self.confirm_delivery(expected, replacement, DeliveryMethod::UnicodeKeystrokes);
        }
        match with_temporary_text(&mut self.clipboard, replacement, || {
            self.target.validate_fallback(expected, fallback)?;
            self.input.paste(fallback.pid)
        }) {
            Ok(()) => self.confirm_delivery(expected, replacement, DeliveryMethod::ClipboardPaste),
            Err(InlineError::ClipboardRestore) => Err(InlineError::ClipboardRestore),
            Err(
                InlineError::ClipboardSnapshot
                | InlineError::ClipboardLimit
                | InlineError::ClipboardWrite
                | InlineError::InputInjection,
            ) => {
                self.target.validate_fallback(expected, fallback)?;
                self.input.type_unicode(fallback.pid, replacement)?;
                self.confirm_delivery(expected, replacement, DeliveryMethod::UnicodeKeystrokes)
            }
            Err(error) => Err(error),
        }
    }

    pub fn replace_whole_draft_preview(
        &mut self,
        expected_text: &str,
        replacement: &str,
        controller_pid: i32,
    ) -> Result<InlineRead, PreviewWriteError> {
        self.ensure_active()
            .map_err(PreviewWriteError::before_write)?;
        self.target
            .replace_whole_draft_preview(expected_text, replacement, controller_pid)
    }

    pub fn restore_whole_draft_preview(
        &mut self,
        expected_previews: &[&str],
        original: &InlineRead,
        controller_pid: i32,
    ) -> Result<InlineRead, InlineError> {
        self.ensure_active()?;
        self.target
            .restore_whole_draft_preview(expected_previews, original, controller_pid)
    }

    fn confirm_delivery(
        &self,
        expected: &InlineRead,
        replacement: &str,
        method: DeliveryMethod,
    ) -> Result<DeliveryMethod, InlineError> {
        if expected.scope == TextScope::WholeDraft {
            self.target.confirm_whole_draft(replacement)?;
        }
        Ok(method)
    }

    pub fn cancel(&mut self) -> Result<(), InlineError> {
        self.cleanup()
    }

    fn cleanup(&mut self) -> Result<(), InlineError> {
        if self.released {
            return Ok(());
        }
        self.target.release();
        self.released = true;
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), InlineError> {
        if self.released {
            Err(InlineError::Released)
        } else {
            Ok(())
        }
    }
}

impl<T, C, I> Drop for InlineSession<T, C, I>
where
    T: FocusedTextTarget,
    C: Clipboard,
    I: InputInjector,
{
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{
        ClipboardItem, ClipboardRepresentation, ClipboardRevision, ClipboardSnapshot, Utf16Range,
    };

    struct FakeTarget {
        attempt: ReplacementAttempt,
        current: InlineRead,
        confirmed_whole_draft: Option<String>,
        focus_valid: bool,
        validations: Arc<Mutex<usize>>,
        released: Arc<Mutex<bool>>,
    }

    impl FocusedTextTarget for FakeTarget {
        fn metadata(&self) -> Result<FocusedElementMetadata, InlineError> {
            Ok(self.current.metadata.clone())
        }

        fn read(&self, _scope: TextScope) -> Result<InlineRead, InlineError> {
            Ok(self.current.clone())
        }

        fn validate(
            &self,
            expected: &InlineRead,
            _focus: DeliveryFocus,
        ) -> Result<(), InlineError> {
            *self.validations.lock().unwrap() += 1;
            if !self.focus_valid {
                return Err(InlineError::TargetFocusChanged);
            }
            if &self.current == expected {
                Ok(())
            } else {
                Err(InlineError::TargetContentChanged)
            }
        }

        fn validate_controller_focus(&self, _controller_pid: i32) -> Result<(), InlineError> {
            if self.focus_valid {
                Ok(())
            } else {
                Err(InlineError::TargetFocusChanged)
            }
        }

        fn replace(
            &mut self,
            expected: &InlineRead,
            _replacement: &str,
            focus: DeliveryFocus,
        ) -> Result<ReplacementAttempt, InlineError> {
            self.validate(expected, focus)?;
            Ok(self.attempt)
        }

        fn replace_whole_draft_preview(
            &mut self,
            expected_text: &str,
            replacement: &str,
            controller_pid: i32,
        ) -> Result<InlineRead, PreviewWriteError> {
            self.validate_controller_focus(controller_pid)
                .map_err(PreviewWriteError::before_write)?;
            if self.current.scope != TextScope::WholeDraft {
                return Err(PreviewWriteError::before_write(InlineError::NotWritable));
            }
            if self.current.text != expected_text {
                return Err(PreviewWriteError::before_write(
                    InlineError::TargetContentChanged,
                ));
            }
            self.current.text = replacement.to_owned();
            let caret = self.current.selection.map(|_| Utf16Range {
                location: replacement.encode_utf16().count(),
                length: 0,
            });
            self.current.selection = caret;
            self.current.metadata.selection = caret;
            Ok(self.current.clone())
        }

        fn restore_whole_draft_preview(
            &mut self,
            expected_previews: &[&str],
            original: &InlineRead,
            controller_pid: i32,
        ) -> Result<InlineRead, InlineError> {
            if self.current.text == original.text {
                return Ok(self.current.clone());
            }
            if !expected_previews.contains(&self.current.text.as_str()) {
                return Err(InlineError::TargetContentChanged);
            }
            self.validate_controller_focus(controller_pid)?;
            self.current.text = original.text.clone();
            self.current.selection = original.selection;
            self.current.metadata.selection = original.metadata.selection;
            Ok(self.current.clone())
        }

        fn prepare_fallback(
            &mut self,
            expected: &InlineRead,
            _hint: WakeHint,
            focus: DeliveryFocus,
        ) -> Result<FallbackTarget, InlineError> {
            self.validate(expected, focus)?;
            Ok(FallbackTarget {
                pid: expected.metadata.pid,
                selection: expected.selection.unwrap(),
            })
        }

        fn validate_fallback(
            &self,
            expected: &InlineRead,
            _fallback: FallbackTarget,
        ) -> Result<(), InlineError> {
            self.validate(expected, DeliveryFocus::Target)
        }

        fn confirm_whole_draft(&self, replacement: &str) -> Result<(), InlineError> {
            if self.current.text == replacement
                || self.confirmed_whole_draft.as_deref() == Some(replacement)
            {
                Ok(())
            } else {
                Err(InlineError::DeliveryUnconfirmed)
            }
        }

        fn release(&mut self) {
            *self.released.lock().unwrap() = true;
        }
    }

    struct FakeClipboard {
        current: Arc<Mutex<ClipboardSnapshot>>,
        revision: u64,
    }

    impl Clipboard for FakeClipboard {
        fn snapshot(&mut self) -> Result<(ClipboardSnapshot, ClipboardRevision), InlineError> {
            Ok((
                self.current.lock().unwrap().clone(),
                ClipboardRevision(self.revision),
            ))
        }

        fn set_plain_text(
            &mut self,
            value: &str,
            expected_revision: &mut ClipboardRevision,
        ) -> Result<(), InlineError> {
            assert_eq!(expected_revision.0, self.revision);
            self.revision += 1;
            expected_revision.0 = self.revision;
            *self.current.lock().unwrap() = ClipboardSnapshot {
                items: vec![ClipboardItem {
                    representations: vec![ClipboardRepresentation {
                        type_identifier: "text".into(),
                        data: value.as_bytes().to_vec(),
                    }],
                }],
            };
            Ok(())
        }

        fn restore(
            &mut self,
            snapshot: &ClipboardSnapshot,
            expected_revision: &mut ClipboardRevision,
        ) -> Result<(), InlineError> {
            assert_eq!(expected_revision.0, self.revision);
            self.revision += 1;
            expected_revision.0 = self.revision;
            *self.current.lock().unwrap() = snapshot.clone();
            Ok(())
        }

        fn ensure_revision(
            &mut self,
            expected_revision: ClipboardRevision,
        ) -> Result<(), InlineError> {
            if self.revision == expected_revision.0 {
                Ok(())
            } else {
                Err(InlineError::ClipboardChanged)
            }
        }
    }

    #[derive(Default)]
    struct InputCounts {
        pasted: usize,
        typed: usize,
    }

    struct FakeInput {
        counts: Arc<Mutex<InputCounts>>,
        paste_error: Option<InlineError>,
    }

    impl InputInjector for FakeInput {
        fn paste(&mut self, _pid: i32) -> Result<(), InlineError> {
            self.counts.lock().unwrap().pasted += 1;
            self.paste_error.map_or(Ok(()), Err)
        }

        fn type_unicode(&mut self, _pid: i32, _value: &str) -> Result<(), InlineError> {
            self.counts.lock().unwrap().typed += 1;
            Ok(())
        }
    }

    fn expected_read(scope: TextScope) -> InlineRead {
        let selection = Some(Utf16Range {
            location: 0,
            length: 7,
        });
        InlineRead {
            scope,
            text: "private draft".into(),
            selection,
            metadata: FocusedElementMetadata {
                pid: 42,
                selection,
                ..FocusedElementMetadata::default()
            },
        }
    }

    fn delivery_session(
        expected: &InlineRead,
        attempt: ReplacementAttempt,
        confirmed_whole_draft: Option<&str>,
        paste_error: Option<InlineError>,
    ) -> (
        InlineSession<FakeTarget, FakeClipboard, FakeInput>,
        Arc<Mutex<InputCounts>>,
    ) {
        let counts = Arc::new(Mutex::new(InputCounts::default()));
        (
            InlineSession::new(
                FakeTarget {
                    attempt,
                    current: expected.clone(),
                    confirmed_whole_draft: confirmed_whole_draft.map(str::to_owned),
                    focus_valid: true,
                    validations: Arc::new(Mutex::new(0)),
                    released: Arc::new(Mutex::new(false)),
                },
                FakeClipboard {
                    current: Arc::new(Mutex::new(ClipboardSnapshot::default())),
                    revision: 1,
                },
                FakeInput {
                    counts: Arc::clone(&counts),
                    paste_error,
                },
            ),
            counts,
        )
    }

    #[test]
    fn whole_draft_preview_and_restore_never_use_clipboard_or_input_fallbacks() {
        let original = expected_read(TextScope::WholeDraft);
        let (mut session, counts) =
            delivery_session(&original, ReplacementAttempt::Unavailable, None, None);

        let preview = session
            .replace_whole_draft_preview(&original.text, "generating.", 7)
            .unwrap();
        assert_eq!(preview.text, "generating.");
        let restored = session
            .restore_whole_draft_preview(&["generating."], &original, 7)
            .unwrap();
        assert_eq!(restored.text, original.text);
        assert_eq!(counts.lock().unwrap().pasted, 0);
        assert_eq!(counts.lock().unwrap().typed, 0);
    }

    #[test]
    fn prefers_accessibility_replacement_without_touching_clipboard() {
        let original = ClipboardSnapshot::default();
        let clipboard_state = Arc::new(Mutex::new(original.clone()));
        let released = Arc::new(Mutex::new(false));
        let validations = Arc::new(Mutex::new(0));
        let input_counts = Arc::new(Mutex::new(InputCounts::default()));
        let expected = expected_read(TextScope::Selection);
        let mut session = InlineSession::new(
            FakeTarget {
                attempt: ReplacementAttempt::SelectedText,
                current: expected.clone(),
                confirmed_whole_draft: None,
                focus_valid: true,
                validations: Arc::clone(&validations),
                released,
            },
            FakeClipboard {
                current: Arc::clone(&clipboard_state),
                revision: 1,
            },
            FakeInput {
                counts: Arc::clone(&input_counts),
                paste_error: None,
            },
        );
        assert_eq!(
            session
                .deliver(
                    &expected,
                    "replacement",
                    WakeHint::Standard,
                    DeliveryFocus::Target,
                )
                .unwrap(),
            DeliveryMethod::AccessibilitySelectedText
        );
        assert_eq!(*clipboard_state.lock().unwrap(), original);
        assert_eq!(input_counts.lock().unwrap().pasted, 0);
        assert_eq!(input_counts.lock().unwrap().typed, 0);
        session.cancel().unwrap();
        assert_eq!(input_counts.lock().unwrap().pasted, 0);
        assert_eq!(input_counts.lock().unwrap().typed, 0);
    }

    #[test]
    fn clipboard_paste_restores_original_and_cleanup_releases_target() {
        let original = ClipboardSnapshot {
            items: vec![ClipboardItem {
                representations: vec![ClipboardRepresentation {
                    type_identifier: "binary".into(),
                    data: vec![0, 1, 2],
                }],
            }],
        };
        let clipboard_state = Arc::new(Mutex::new(original.clone()));
        let released = Arc::new(Mutex::new(false));
        let validations = Arc::new(Mutex::new(0));
        let input_counts = Arc::new(Mutex::new(InputCounts::default()));
        let expected = expected_read(TextScope::WholeDraft);
        let mut session = InlineSession::new(
            FakeTarget {
                attempt: ReplacementAttempt::Unavailable,
                current: expected.clone(),
                confirmed_whole_draft: Some("replacement".into()),
                focus_valid: true,
                validations: Arc::clone(&validations),
                released: Arc::clone(&released),
            },
            FakeClipboard {
                current: Arc::clone(&clipboard_state),
                revision: 1,
            },
            FakeInput {
                counts: Arc::clone(&input_counts),
                paste_error: None,
            },
        );
        assert_eq!(
            session
                .deliver(
                    &expected,
                    "replacement",
                    WakeHint::GmailContentEditable,
                    DeliveryFocus::Target,
                )
                .unwrap(),
            DeliveryMethod::ClipboardPaste
        );
        assert_eq!(*clipboard_state.lock().unwrap(), original);
        assert_eq!(input_counts.lock().unwrap().pasted, 1);
        session.cancel().unwrap();
        assert!(*released.lock().unwrap());
        assert!(matches!(
            session.read(TextScope::WholeDraft),
            Err(InlineError::Released)
        ));
    }

    #[test]
    fn ignored_accessibility_whole_draft_write_is_not_reported_as_delivered() {
        let expected = expected_read(TextScope::WholeDraft);
        let (mut session, counts) =
            delivery_session(&expected, ReplacementAttempt::Value, None, None);

        assert_eq!(
            session.deliver(
                &expected,
                "replacement",
                WakeHint::Standard,
                DeliveryFocus::Target,
            ),
            Err(InlineError::DeliveryUnconfirmed)
        );
        assert_eq!(counts.lock().unwrap().pasted, 0);
        assert_eq!(counts.lock().unwrap().typed, 0);
    }

    #[test]
    fn dropped_clipboard_paste_is_not_reported_as_delivered() {
        let expected = expected_read(TextScope::WholeDraft);
        let (mut session, counts) =
            delivery_session(&expected, ReplacementAttempt::Unavailable, None, None);

        assert_eq!(
            session.deliver(
                &expected,
                "replacement",
                WakeHint::Standard,
                DeliveryFocus::Target,
            ),
            Err(InlineError::DeliveryUnconfirmed)
        );
        assert_eq!(counts.lock().unwrap().pasted, 1);
        assert_eq!(counts.lock().unwrap().typed, 0);
    }

    #[test]
    fn ignored_unicode_fallback_is_not_reported_as_delivered() {
        let expected = expected_read(TextScope::WholeDraft);
        let (mut session, counts) = delivery_session(
            &expected,
            ReplacementAttempt::Unavailable,
            None,
            Some(InlineError::InputInjection),
        );

        assert_eq!(
            session.deliver(
                &expected,
                "replacement",
                WakeHint::Standard,
                DeliveryFocus::Target,
            ),
            Err(InlineError::DeliveryUnconfirmed)
        );
        assert_eq!(counts.lock().unwrap().pasted, 1);
        assert_eq!(counts.lock().unwrap().typed, 1);
    }

    #[test]
    fn stale_content_is_rejected_before_any_fallback_or_input() {
        let expected = expected_read(TextScope::Selection);
        let mut current = expected.clone();
        current.text = "changed elsewhere".into();
        let released = Arc::new(Mutex::new(false));
        let counts = Arc::new(Mutex::new(InputCounts::default()));
        let mut session = InlineSession::new(
            FakeTarget {
                attempt: ReplacementAttempt::Unavailable,
                current,
                confirmed_whole_draft: None,
                focus_valid: true,
                validations: Arc::new(Mutex::new(0)),
                released,
            },
            FakeClipboard {
                current: Arc::new(Mutex::new(ClipboardSnapshot::default())),
                revision: 1,
            },
            FakeInput {
                counts: Arc::clone(&counts),
                paste_error: None,
            },
        );
        assert_eq!(
            session.deliver(
                &expected,
                "replacement",
                WakeHint::Standard,
                DeliveryFocus::Target,
            ),
            Err(InlineError::TargetContentChanged)
        );
        assert_eq!(counts.lock().unwrap().pasted, 0);
        assert_eq!(counts.lock().unwrap().typed, 0);
    }

    #[test]
    fn stale_focus_is_rejected_before_any_mutation_or_input() {
        let expected = expected_read(TextScope::Selection);
        let counts = Arc::new(Mutex::new(InputCounts::default()));
        let mut session = InlineSession::new(
            FakeTarget {
                attempt: ReplacementAttempt::SelectedText,
                current: expected.clone(),
                confirmed_whole_draft: None,
                focus_valid: false,
                validations: Arc::new(Mutex::new(0)),
                released: Arc::new(Mutex::new(false)),
            },
            FakeClipboard {
                current: Arc::new(Mutex::new(ClipboardSnapshot::default())),
                revision: 1,
            },
            FakeInput {
                counts: Arc::clone(&counts),
                paste_error: None,
            },
        );
        assert_eq!(
            session.deliver(
                &expected,
                "replacement",
                WakeHint::Standard,
                DeliveryFocus::Target,
            ),
            Err(InlineError::TargetFocusChanged)
        );
        assert_eq!(counts.lock().unwrap().pasted, 0);
        assert_eq!(counts.lock().unwrap().typed, 0);
    }

    #[test]
    fn debug_output_never_contains_read_text() {
        let read = InlineRead {
            scope: TextScope::WholeDraft,
            text: "highly private draft".into(),
            selection: None,
            metadata: FocusedElementMetadata::default(),
        };
        let debug = format!("{read:?}");
        assert!(!debug.contains("highly private draft"));
        assert!(debug.contains("[REDACTED]"));
    }
}
