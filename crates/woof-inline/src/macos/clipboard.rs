use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};

use crate::{
    clipboard::{
        validate_snapshot_limits, MAX_CLIPBOARD_ITEMS, MAX_CLIPBOARD_TOTAL_BYTES,
        MAX_CLIPBOARD_TYPES_PER_ITEM, MAX_CLIPBOARD_TYPE_BYTES,
    },
    Clipboard, ClipboardItem, ClipboardRepresentation, ClipboardRevision, ClipboardSnapshot,
    InlineError,
};

#[derive(Clone, Copy, Debug, Default)]
pub struct MacOsClipboard;

impl Clipboard for MacOsClipboard {
    fn snapshot(&mut self) -> Result<(ClipboardSnapshot, ClipboardRevision), InlineError> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let revision = pasteboard_revision(&pasteboard)?;
        let Some(items) = pasteboard.pasteboardItems() else {
            return unchanged_snapshot(&pasteboard, ClipboardSnapshot::default(), revision);
        };
        if items.len() > MAX_CLIPBOARD_ITEMS {
            return Err(InlineError::ClipboardLimit);
        }
        let mut snapshot_items = Vec::with_capacity(items.len());
        let mut total_bytes = 0_usize;
        for item_index in 0..items.len() {
            // SAFETY: `item_index` is bounded by the immutable NSArray length.
            let item = unsafe { items.objectAtIndex_unchecked(item_index) };
            let types = item.types();
            if types.len() > MAX_CLIPBOARD_TYPES_PER_ITEM {
                return Err(InlineError::ClipboardLimit);
            }
            let mut representations = Vec::with_capacity(types.len());
            for type_index in 0..types.len() {
                // SAFETY: `type_index` is bounded by the immutable NSArray length.
                let data_type = unsafe { types.objectAtIndex_unchecked(type_index) };
                let type_identifier = data_type.to_string();
                if type_identifier.len() > MAX_CLIPBOARD_TYPE_BYTES {
                    return Err(InlineError::ClipboardLimit);
                }
                let data = item
                    .dataForType(data_type)
                    .ok_or(InlineError::ClipboardSnapshot)?;
                total_bytes = total_bytes
                    .checked_add(type_identifier.len())
                    .and_then(|total| total.checked_add(data.len()))
                    .ok_or(InlineError::ClipboardLimit)?;
                if total_bytes > MAX_CLIPBOARD_TOTAL_BYTES {
                    return Err(InlineError::ClipboardLimit);
                }
                representations.push(ClipboardRepresentation {
                    type_identifier,
                    data: data.to_vec(),
                });
            }
            snapshot_items.push(ClipboardItem { representations });
        }
        let snapshot = ClipboardSnapshot {
            items: snapshot_items,
        };
        validate_snapshot_limits(&snapshot)?;
        unchanged_snapshot(&pasteboard, snapshot, revision)
    }

    fn set_plain_text(
        &mut self,
        value: &str,
        expected_revision: &mut ClipboardRevision,
    ) -> Result<(), InlineError> {
        let pasteboard = NSPasteboard::generalPasteboard();
        ensure_revision(&pasteboard, *expected_revision)?;
        *expected_revision = revision_from_count(pasteboard.clearContents())?;
        ensure_revision(&pasteboard, *expected_revision)?;
        let value = NSString::from_str(value);
        let written = pasteboard.setString_forType(&value, unsafe { NSPasteboardTypeString });
        ensure_revision(&pasteboard, *expected_revision)?;
        if written {
            Ok(())
        } else {
            Err(InlineError::ClipboardWrite)
        }
    }

    fn restore(
        &mut self,
        snapshot: &ClipboardSnapshot,
        expected_revision: &mut ClipboardRevision,
    ) -> Result<(), InlineError> {
        validate_snapshot_limits(snapshot)?;
        // Construct every native object before touching the global pasteboard.
        // If allocation or type restoration fails, the temporary clipboard is
        // still intact and a later guard retry remains safe.
        let objects = native_items(snapshot)?;
        let pasteboard = NSPasteboard::generalPasteboard();
        ensure_revision(&pasteboard, *expected_revision)?;
        *expected_revision = revision_from_count(pasteboard.clearContents())?;
        if snapshot.items.is_empty() {
            return Ok(());
        }
        ensure_revision(&pasteboard, *expected_revision)?;
        let written = pasteboard.writeObjects(&objects);
        ensure_revision(&pasteboard, *expected_revision)?;
        if written {
            Ok(())
        } else {
            Err(InlineError::ClipboardRestore)
        }
    }

    fn ensure_revision(&mut self, expected_revision: ClipboardRevision) -> Result<(), InlineError> {
        ensure_revision(&NSPasteboard::generalPasteboard(), expected_revision)
    }
}

fn pasteboard_revision(pasteboard: &NSPasteboard) -> Result<ClipboardRevision, InlineError> {
    revision_from_count(pasteboard.changeCount())
}

fn revision_from_count(change_count: isize) -> Result<ClipboardRevision, InlineError> {
    u64::try_from(change_count)
        .map(ClipboardRevision)
        .map_err(|_| InlineError::ClipboardSnapshot)
}

fn ensure_revision(
    pasteboard: &NSPasteboard,
    expected: ClipboardRevision,
) -> Result<(), InlineError> {
    if pasteboard_revision(pasteboard)? == expected {
        Ok(())
    } else {
        Err(InlineError::ClipboardChanged)
    }
}

fn unchanged_snapshot(
    pasteboard: &NSPasteboard,
    snapshot: ClipboardSnapshot,
    revision: ClipboardRevision,
) -> Result<(ClipboardSnapshot, ClipboardRevision), InlineError> {
    ensure_revision(pasteboard, revision)?;
    Ok((snapshot, revision))
}

fn native_items(
    snapshot: &ClipboardSnapshot,
) -> Result<Retained<NSArray<ProtocolObject<dyn NSPasteboardWriting>>>, InlineError> {
    let mut native_items = Vec::with_capacity(snapshot.items.len());
    for item in &snapshot.items {
        let native = NSPasteboardItem::new();
        for representation in &item.representations {
            let data_type = NSString::from_str(&representation.type_identifier);
            let data = NSData::with_bytes(&representation.data);
            if !native.setData_forType(&data, &data_type) {
                return Err(InlineError::ClipboardRestore);
            }
        }
        native_items.push(ProtocolObject::<dyn NSPasteboardWriting>::from_retained(
            native,
        ));
    }
    Ok(NSArray::from_retained_slice(&native_items))
}
