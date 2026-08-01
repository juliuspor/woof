use std::fmt;

use crate::InlineError;

pub(crate) const MAX_CLIPBOARD_ITEMS: usize = 64;
pub(crate) const MAX_CLIPBOARD_TYPES_PER_ITEM: usize = 128;
pub(crate) const MAX_CLIPBOARD_TYPE_BYTES: usize = 1_024;
pub(crate) const MAX_CLIPBOARD_TOTAL_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub struct ClipboardRepresentation {
    pub type_identifier: String,
    pub data: Vec<u8>,
}

impl fmt::Debug for ClipboardRepresentation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClipboardRepresentation")
            .field("type_identifier", &self.type_identifier)
            .field("data", &"[REDACTED]")
            .field("bytes", &self.data.len())
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct ClipboardItem {
    pub representations: Vec<ClipboardRepresentation>,
}

impl fmt::Debug for ClipboardItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClipboardItem")
            .field("representations", &self.representations)
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    pub items: Vec<ClipboardItem>,
}

impl fmt::Debug for ClipboardSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClipboardSnapshot")
            .field("items", &self.items)
            .finish()
    }
}

/// Monotonic clipboard revision captured at the native pasteboard boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClipboardRevision(pub u64);

pub trait Clipboard: Send {
    fn snapshot(&mut self) -> Result<(ClipboardSnapshot, ClipboardRevision), InlineError>;
    fn set_plain_text(
        &mut self,
        value: &str,
        expected_revision: &mut ClipboardRevision,
    ) -> Result<(), InlineError>;
    fn ensure_revision(&mut self, expected_revision: ClipboardRevision) -> Result<(), InlineError>;
    fn restore(
        &mut self,
        snapshot: &ClipboardSnapshot,
        expected_revision: &mut ClipboardRevision,
    ) -> Result<(), InlineError>;
}

/// Runs an action with temporary plain text on the clipboard and restores
/// every original item/type payload before returning.
pub fn with_temporary_text<C, F, T>(
    clipboard: &mut C,
    value: &str,
    action: F,
) -> Result<T, InlineError>
where
    C: Clipboard,
    F: FnOnce() -> Result<T, InlineError>,
{
    let (snapshot, revision) = clipboard.snapshot()?;
    validate_snapshot_limits(&snapshot)?;
    let mut restoration = ClipboardRestoration::new(clipboard, snapshot, revision);
    if let Err(write_error) = restoration
        .clipboard
        .set_plain_text(value, &mut restoration.revision)
    {
        return match restoration.restore() {
            Ok(()) => Err(write_error),
            Err(InlineError::ClipboardChanged) => Err(InlineError::ClipboardChanged),
            Err(_) => Err(InlineError::ClipboardRestore),
        };
    }
    restoration
        .clipboard
        .ensure_revision(restoration.revision)?;
    let action_result = action();
    let restore_result = restoration.restore();
    match (action_result, restore_result) {
        (_, Err(InlineError::ClipboardChanged)) => Err(InlineError::ClipboardChanged),
        (_, Err(_)) => Err(InlineError::ClipboardRestore),
        (result, Ok(())) => result,
    }
}

pub(crate) fn validate_snapshot_limits(snapshot: &ClipboardSnapshot) -> Result<(), InlineError> {
    if snapshot.items.len() > MAX_CLIPBOARD_ITEMS {
        return Err(InlineError::ClipboardLimit);
    }
    let mut total_bytes = 0_usize;
    for item in &snapshot.items {
        if item.representations.len() > MAX_CLIPBOARD_TYPES_PER_ITEM {
            return Err(InlineError::ClipboardLimit);
        }
        for representation in &item.representations {
            if representation.type_identifier.len() > MAX_CLIPBOARD_TYPE_BYTES {
                return Err(InlineError::ClipboardLimit);
            }
            total_bytes = total_bytes
                .checked_add(representation.type_identifier.len())
                .and_then(|total| total.checked_add(representation.data.len()))
                .ok_or(InlineError::ClipboardLimit)?;
            if total_bytes > MAX_CLIPBOARD_TOTAL_BYTES {
                return Err(InlineError::ClipboardLimit);
            }
        }
    }
    Ok(())
}

struct ClipboardRestoration<'a, C>
where
    C: Clipboard,
{
    clipboard: &'a mut C,
    snapshot: Option<ClipboardSnapshot>,
    revision: ClipboardRevision,
}

impl<'a, C> ClipboardRestoration<'a, C>
where
    C: Clipboard,
{
    fn new(clipboard: &'a mut C, snapshot: ClipboardSnapshot, revision: ClipboardRevision) -> Self {
        Self {
            clipboard,
            snapshot: Some(snapshot),
            revision,
        }
    }

    fn restore(&mut self) -> Result<(), InlineError> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Ok(());
        };
        self.clipboard.restore(snapshot, &mut self.revision)?;
        self.snapshot.take();
        Ok(())
    }
}

impl<C> Drop for ClipboardRestoration<'_, C>
where
    C: Clipboard,
{
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use std::{
        panic::{catch_unwind, AssertUnwindSafe},
        sync::{Arc, Mutex},
    };

    use super::*;

    struct MemoryClipboard {
        state: Arc<Mutex<MemoryClipboardState>>,
        restore_failures: usize,
    }

    struct MemoryClipboardState {
        current: ClipboardSnapshot,
        revision: u64,
    }

    impl Clipboard for MemoryClipboard {
        fn snapshot(&mut self) -> Result<(ClipboardSnapshot, ClipboardRevision), InlineError> {
            let state = self.state.lock().unwrap();
            Ok((state.current.clone(), ClipboardRevision(state.revision)))
        }

        fn set_plain_text(
            &mut self,
            value: &str,
            expected_revision: &mut ClipboardRevision,
        ) -> Result<(), InlineError> {
            let mut state = self.state.lock().unwrap();
            if state.revision != expected_revision.0 {
                return Err(InlineError::ClipboardChanged);
            }
            state.revision += 1;
            expected_revision.0 = state.revision;
            state.current = ClipboardSnapshot {
                items: vec![ClipboardItem {
                    representations: vec![ClipboardRepresentation {
                        type_identifier: "public.utf8-plain-text".into(),
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
            let mut state = self.state.lock().unwrap();
            if state.revision != expected_revision.0 {
                return Err(InlineError::ClipboardChanged);
            }
            state.revision += 1;
            expected_revision.0 = state.revision;
            if self.restore_failures > 0 {
                self.restore_failures -= 1;
                return Err(InlineError::ClipboardRestore);
            }
            state.current = snapshot.clone();
            Ok(())
        }

        fn ensure_revision(
            &mut self,
            expected_revision: ClipboardRevision,
        ) -> Result<(), InlineError> {
            if self.state.lock().unwrap().revision == expected_revision.0 {
                Ok(())
            } else {
                Err(InlineError::ClipboardChanged)
            }
        }
    }

    fn memory_clipboard(snapshot: ClipboardSnapshot) -> MemoryClipboard {
        MemoryClipboard {
            state: Arc::new(Mutex::new(MemoryClipboardState {
                current: snapshot,
                revision: 1,
            })),
            restore_failures: 0,
        }
    }

    fn rich_snapshot() -> ClipboardSnapshot {
        ClipboardSnapshot {
            items: vec![
                ClipboardItem {
                    representations: vec![
                        ClipboardRepresentation {
                            type_identifier: "public.utf8-plain-text".into(),
                            data: b"private text".to_vec(),
                        },
                        ClipboardRepresentation {
                            type_identifier: "public.rtf".into(),
                            data: vec![0, 1, 2, 3],
                        },
                    ],
                },
                ClipboardItem {
                    representations: vec![ClipboardRepresentation {
                        type_identifier: "public.png".into(),
                        data: vec![137, 80, 78, 71],
                    }],
                },
            ],
        }
    }

    #[test]
    fn restores_every_item_and_representation_after_success() {
        let original = rich_snapshot();
        let mut clipboard = memory_clipboard(original.clone());
        let result =
            with_temporary_text(&mut clipboard, "replacement", || Ok::<_, InlineError>(7)).unwrap();
        assert_eq!(result, 7);
        assert_eq!(clipboard.state.lock().unwrap().current, original);
    }

    #[test]
    fn restores_exact_clipboard_after_action_failure() {
        let original = rich_snapshot();
        let mut clipboard = memory_clipboard(original.clone());
        let result = with_temporary_text(&mut clipboard, "replacement", || {
            Err::<(), _>(InlineError::InputInjection)
        });
        assert_eq!(result, Err(InlineError::InputInjection));
        assert_eq!(clipboard.state.lock().unwrap().current, original);
    }

    #[test]
    fn debug_redacts_clipboard_bytes() {
        let debug = format!("{:?}", rich_snapshot());
        assert!(!debug.contains("private text"));
        assert!(debug.contains("[REDACTED]"));
    }

    #[test]
    fn panic_cleanup_still_restores_exact_clipboard() {
        let original = rich_snapshot();
        let mut clipboard = memory_clipboard(original.clone());
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = with_temporary_text(
                &mut clipboard,
                "replacement",
                || -> Result<(), InlineError> {
                    panic!("synthetic action panic");
                },
            );
        }));
        assert!(result.is_err());
        assert_eq!(clipboard.state.lock().unwrap().current, original);
    }

    #[test]
    fn refuses_an_oversized_snapshot_before_mutating_the_clipboard() {
        let oversized = ClipboardSnapshot {
            items: vec![ClipboardItem {
                representations: vec![ClipboardRepresentation {
                    type_identifier: "public.data".into(),
                    data: vec![0; MAX_CLIPBOARD_TOTAL_BYTES + 1],
                }],
            }],
        };
        let mut clipboard = memory_clipboard(oversized);
        let before_revision = clipboard.state.lock().unwrap().revision;
        assert_eq!(
            with_temporary_text(&mut clipboard, "replacement", || Ok(())),
            Err(InlineError::ClipboardLimit)
        );
        assert_eq!(clipboard.state.lock().unwrap().revision, before_revision);
    }

    #[test]
    fn retained_snapshot_allows_a_failed_restore_to_retry_on_drop() {
        let original = rich_snapshot();
        let mut clipboard = memory_clipboard(original.clone());
        clipboard.restore_failures = 1;
        assert_eq!(
            with_temporary_text(&mut clipboard, "replacement", || Ok(())),
            Err(InlineError::ClipboardRestore)
        );
        assert_eq!(clipboard.state.lock().unwrap().current, original);
        assert_eq!(clipboard.restore_failures, 0);
    }

    #[test]
    fn concurrent_clipboard_change_is_never_overwritten() {
        let original = rich_snapshot();
        let mut clipboard = memory_clipboard(original);
        let external_state = Arc::clone(&clipboard.state);
        let external = ClipboardSnapshot {
            items: vec![ClipboardItem {
                representations: vec![ClipboardRepresentation {
                    type_identifier: "public.utf8-plain-text".into(),
                    data: b"new external value".to_vec(),
                }],
            }],
        };
        assert_eq!(
            with_temporary_text(&mut clipboard, "replacement", || {
                let mut state = external_state.lock().unwrap();
                state.revision += 1;
                state.current = external.clone();
                Ok(())
            }),
            Err(InlineError::ClipboardChanged)
        );
        assert_eq!(clipboard.state.lock().unwrap().current, external);
    }
}
