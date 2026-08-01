use tauri::{AppHandle, Manager};
use woof_inline::{
    DeliveryFocus, DeliveryMethod, FocusedElementMetadata, InlineError, InlineRead, InlineSession,
    MacOsClipboard, MacOsFocusedTarget, MacOsInputInjector, ModifierConfig, ModifierMonitor, Rect,
    TextScope, WakeHint,
};
use woof_llm::CancellationToken;

use crate::{commands, state::UiState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationMode {
    Rewrite,
    Dictation,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FocusDecision {
    Editable {
        session_id: u64,
        frame: Option<Rect>,
        scope: TextScope,
    },
    NonEditable,
}

pub struct RewriteSnapshot {
    pub session_id: u64,
    pub original: String,
    pub app: String,
    pub domain: String,
    pub cancellation: CancellationToken,
}

pub struct DeliveryReceipt {
    pub method: DeliveryMethod,
    pub scope: TextScope,
    pub app: String,
    pub domain: String,
}

pub struct DictationDelivery {
    pub receipt: DeliveryReceipt,
    pub transcript: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineSessionSnapshot {
    pub session_id: u64,
    pub status: &'static str,
}

trait RuntimeInlineSession: Send {
    fn metadata(&self) -> Result<FocusedElementMetadata, InlineError>;
    fn read(&self, scope: TextScope) -> Result<InlineRead, InlineError>;
    fn deliver(
        &mut self,
        expected: &InlineRead,
        replacement: &str,
        wake_hint: WakeHint,
        focus: DeliveryFocus,
    ) -> Result<DeliveryMethod, InlineError>;
    fn cancel(&mut self) -> Result<(), InlineError>;
}

trait FocusSessionFactory {
    fn capture(&mut self) -> Result<Box<dyn RuntimeInlineSession>, InlineError>;
}

type NativeSession = InlineSession<MacOsFocusedTarget, MacOsClipboard, MacOsInputInjector>;

struct NativeRuntimeSession {
    inner: NativeSession,
}

impl RuntimeInlineSession for NativeRuntimeSession {
    fn metadata(&self) -> Result<FocusedElementMetadata, InlineError> {
        self.inner.metadata()
    }

    fn read(&self, scope: TextScope) -> Result<InlineRead, InlineError> {
        self.inner.read(scope)
    }

    fn deliver(
        &mut self,
        expected: &InlineRead,
        replacement: &str,
        wake_hint: WakeHint,
        focus: DeliveryFocus,
    ) -> Result<DeliveryMethod, InlineError> {
        self.inner.deliver(expected, replacement, wake_hint, focus)
    }

    fn cancel(&mut self) -> Result<(), InlineError> {
        self.inner.cancel()
    }
}

struct NativeFocusFactory;

impl FocusSessionFactory for NativeFocusFactory {
    fn capture(&mut self) -> Result<Box<dyn RuntimeInlineSession>, InlineError> {
        let target = MacOsFocusedTarget::acquire()?;
        Ok(Box::new(NativeRuntimeSession {
            inner: InlineSession::new(target, MacOsClipboard, MacOsInputInjector),
        }))
    }
}

struct ActiveInlineSession {
    id: u64,
    mode: ActivationMode,
    session: Box<dyn RuntimeInlineSession>,
    expected: InlineRead,
    wake_hint: WakeHint,
    app: String,
    domain: String,
    cancellation: CancellationToken,
    rewrite_in_flight: bool,
    pending_transcript: Option<String>,
}

#[derive(Default)]
pub struct InlineCoordinator {
    next_id: u64,
    active: Option<ActiveInlineSession>,
    modifier_hold_active: bool,
    modifier_transcription_id: Option<u64>,
}

impl InlineCoordinator {
    pub fn begin_native(&mut self, mode: ActivationMode) -> Result<FocusDecision, InlineError> {
        self.begin_with(&mut NativeFocusFactory, mode)
    }

    fn begin_with(
        &mut self,
        factory: &mut dyn FocusSessionFactory,
        mode: ActivationMode,
    ) -> Result<FocusDecision, InlineError> {
        self.cancel_active()?;
        let mut session = factory.capture()?;
        let metadata = match session.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = session.cancel();
                return Err(error);
            }
        };
        if !is_editable(&metadata) {
            let _ = session.cancel();
            return Ok(FocusDecision::NonEditable);
        }

        let expected = match mode {
            ActivationMode::Rewrite => {
                let selected = match session.read(TextScope::Selection) {
                    Ok(read) => read,
                    Err(error) => {
                        let _ = session.cancel();
                        return Err(error);
                    }
                };
                if !selected.text.is_empty() {
                    selected
                } else {
                    match session.read(TextScope::WholeDraft) {
                        Ok(draft) => draft,
                        Err(error) => {
                            let _ = session.cancel();
                            return Err(error);
                        }
                    }
                }
            }
            ActivationMode::Dictation => match session.read(TextScope::Selection) {
                Ok(read) => read,
                Err(error) => {
                    let _ = session.cancel();
                    return Err(error);
                }
            },
        };

        self.next_id = self.next_id.wrapping_add(1).max(1);
        let session_id = self.next_id;
        let wake_hint = wake_hint(&expected.metadata);
        let app = expected
            .metadata
            .bundle_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| expected.metadata.role.clone());
        let domain = if wake_hint == WakeHint::GmailContentEditable {
            "mail.google.com".to_string()
        } else {
            String::new()
        };
        let frame = expected.metadata.frame;
        let scope = expected.scope;
        self.active = Some(ActiveInlineSession {
            id: session_id,
            mode,
            session,
            expected,
            wake_hint,
            app,
            domain,
            cancellation: CancellationToken::new(),
            rewrite_in_flight: false,
            pending_transcript: None,
        });
        Ok(FocusDecision::Editable {
            session_id,
            frame,
            scope,
        })
    }

    pub fn prepare_rewrite(
        &mut self,
        requested_scope: TextScope,
    ) -> Result<RewriteSnapshot, &'static str> {
        let active = self
            .active
            .as_mut()
            .ok_or("the focused inline target is no longer available")?;
        if active.mode != ActivationMode::Rewrite {
            return Err("the focused inline target is being used for dictation");
        }
        if active.rewrite_in_flight {
            return Err("an inline rewrite is already in progress");
        }

        // The current UI initializes its scope control to selection. Keep the
        // activation-time whole-draft fallback authoritative unless the user
        // explicitly asks to expand an actual selection to the whole draft.
        let scope = match (active.expected.scope, requested_scope) {
            (TextScope::WholeDraft, TextScope::Selection) => TextScope::WholeDraft,
            (_, scope) => scope,
        };
        let expected = if active.expected.scope == scope {
            active.expected.clone()
        } else {
            active.session.read(scope).map_err(inline_read_error)?
        };
        active.expected = expected;
        active.rewrite_in_flight = true;
        Ok(RewriteSnapshot {
            session_id: active.id,
            original: active.expected.text.clone(),
            app: active.app.clone(),
            domain: active.domain.clone(),
            cancellation: active.cancellation.clone(),
        })
    }

    pub fn rewrite_failed(&mut self, session_id: u64) {
        if let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.id == session_id)
        {
            active.rewrite_in_flight = false;
        }
    }

    pub fn deliver_rewrite(
        &mut self,
        session_id: u64,
        replacement: &str,
        focus: DeliveryFocus,
    ) -> Result<DeliveryReceipt, InlineError> {
        let mut active = self.take_matching(session_id, ActivationMode::Rewrite)?;
        active.cancellation.cancel();
        deliver_and_release(&mut active, replacement, focus)
    }

    pub fn stage_dictation(&mut self, transcript: String) {
        if let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.mode == ActivationMode::Dictation)
        {
            active.pending_transcript = Some(transcript);
        }
    }

    pub fn session_snapshot(&self) -> Option<InlineSessionSnapshot> {
        self.active.as_ref().map(|active| InlineSessionSnapshot {
            session_id: active.id,
            status: match (active.mode, active.rewrite_in_flight) {
                (ActivationMode::Rewrite, true) => "Working on it…",
                (ActivationMode::Rewrite, false) => "Selection ready",
                (ActivationMode::Dictation, _) => "Listening…",
            },
        })
    }

    pub fn has_session(&self, session_id: u64) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.id == session_id)
    }

    pub fn has_rewrite_session(&self, session_id: u64) -> bool {
        self.active.as_ref().is_some_and(|active| {
            active.id == session_id
                && active.mode == ActivationMode::Rewrite
                && active.rewrite_in_flight
        })
    }

    pub fn complete_dictation(&mut self) -> Result<Option<DictationDelivery>, InlineError> {
        self.modifier_transcription_id = None;
        let Some(active) = self
            .active
            .as_ref()
            .filter(|active| active.mode == ActivationMode::Dictation)
        else {
            return Ok(None);
        };
        let session_id = active.id;
        let mut active = self.take_matching(session_id, ActivationMode::Dictation)?;
        let Some(transcript) = active.pending_transcript.take() else {
            let cleanup = active.session.cancel();
            active.cancellation.cancel();
            cleanup?;
            return Ok(None);
        };
        if transcript.trim().is_empty() {
            let cleanup = active.session.cancel();
            active.cancellation.cancel();
            cleanup?;
            return Ok(None);
        }
        let receipt = deliver_and_release(&mut active, &transcript, DeliveryFocus::Target)?;
        Ok(Some(DictationDelivery {
            receipt,
            transcript,
        }))
    }

    pub fn cancel_all(&mut self) -> Result<(), InlineError> {
        self.modifier_hold_active = false;
        self.modifier_transcription_id = None;
        self.cancel_active()
    }

    pub fn cancel_dictation(&mut self) -> Result<(), InlineError> {
        self.modifier_hold_active = false;
        self.modifier_transcription_id = None;
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.mode == ActivationMode::Dictation)
        {
            self.cancel_active()
        } else {
            Ok(())
        }
    }

    pub fn begin_modifier_hold(&mut self) -> bool {
        if self.modifier_hold_active || self.modifier_transcription_id.is_some() {
            return false;
        }
        self.modifier_hold_active = true;
        true
    }

    pub fn attach_modifier_transcription(&mut self, session_id: u64) {
        self.modifier_transcription_id = Some(session_id);
    }

    pub fn release_modifier_hold(&mut self) -> Option<u64> {
        if !std::mem::replace(&mut self.modifier_hold_active, false) {
            return None;
        }
        self.modifier_transcription_id
    }

    pub fn take_modifier_transcription(&mut self) -> Option<u64> {
        self.modifier_hold_active = false;
        self.modifier_transcription_id.take()
    }

    fn cancel_active(&mut self) -> Result<(), InlineError> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        active.cancellation.cancel();
        active.session.cancel()
    }

    fn take_matching(
        &mut self,
        session_id: u64,
        mode: ActivationMode,
    ) -> Result<ActiveInlineSession, InlineError> {
        let Some(active) = self.active.take() else {
            return Err(InlineError::Released);
        };
        if active.id != session_id || active.mode != mode {
            self.active = Some(active);
            return Err(InlineError::Released);
        }
        Ok(active)
    }
}

fn deliver_and_release(
    active: &mut ActiveInlineSession,
    replacement: &str,
    focus: DeliveryFocus,
) -> Result<DeliveryReceipt, InlineError> {
    let delivery = active
        .session
        .deliver(&active.expected, replacement, active.wake_hint, focus);
    let release = active.session.cancel();
    active.cancellation.cancel();
    let method = match (delivery, release) {
        (Ok(method), Ok(())) => method,
        (Err(error), _) | (Ok(_), Err(error)) => return Err(error),
    };
    Ok(DeliveryReceipt {
        method,
        scope: active.expected.scope,
        app: active.app.clone(),
        domain: active.domain.clone(),
    })
}

fn inline_read_error(error: InlineError) -> &'static str {
    match error {
        InlineError::SecureInput | InlineError::ProtectedContent => {
            "the focused field is protected"
        }
        InlineError::TextUnavailable => "the focused draft cannot be read",
        _ => "the focused inline target is unavailable",
    }
}

fn is_editable(metadata: &FocusedElementMetadata) -> bool {
    metadata.selected_text_writable || metadata.value_writable || metadata.contenteditable
}

fn wake_hint(metadata: &FocusedElementMetadata) -> WakeHint {
    if !metadata.contenteditable {
        return WakeHint::Standard;
    }
    let is_gmail_hint = [
        metadata.title.as_deref(),
        metadata.description.as_deref(),
        metadata.identifier.as_deref(),
        metadata.subrole.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| {
        let value = value.to_ascii_lowercase();
        value.contains("gmail")
            || value.contains("message body")
            || value.contains("messagebody")
            || value.contains("compose body")
    });
    if is_gmail_hint {
        WakeHint::GmailContentEditable
    } else {
        WakeHint::Standard
    }
}

pub fn ensure_modifier_monitor(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<UiState>();
    let preferences = state.read()?;
    let config = modifier_config(&preferences);
    drop(preferences);
    if config.inline_key.is_none() && config.hold_to_talk_key.is_none() {
        stop_modifier_monitor(app);
        return Ok(());
    }
    if state
        .modifier_monitor
        .lock()
        .map_err(|_| "modifier monitor state is unavailable")?
        .is_some()
    {
        return Ok(());
    }
    install_modifier_monitor(app)
}

pub fn install_modifier_monitor(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<UiState>();
    let preferences = state.read()?;
    let config = modifier_config(&preferences);
    drop(preferences);
    if config.inline_key.is_none() && config.hold_to_talk_key.is_none() {
        stop_modifier_monitor(app);
        return Ok(());
    }

    let event_app = app.clone();
    // Start the replacement before swapping it into state. A failed TCC/event
    // tap setup therefore leaves the prior working monitor untouched.
    let replacement = ModifierMonitor::start(config, move |event| {
        commands::handle_modifier_event(event_app.clone(), event);
    })
    .map_err(|error| error.to_string())?;
    let previous = state
        .modifier_monitor
        .lock()
        .map_err(|_| "modifier monitor state is unavailable")?
        .replace(replacement);
    drop(previous);
    Ok(())
}

fn modifier_config(preferences: &crate::state::Preferences) -> ModifierConfig {
    ModifierConfig {
        inline_key: preferences
            .woof_modifier_enabled
            .then_some(preferences.woof_modifier_key),
        hold_to_talk_key: preferences
            .voice_dictation_enabled
            .then_some(preferences.transcription_modifier_key),
        ..ModifierConfig::default()
    }
}

pub fn stop_modifier_monitor(app: &AppHandle) {
    let state = app.state::<UiState>();
    let monitor = state
        .modifier_monitor
        .lock()
        .ok()
        .and_then(|mut monitor| monitor.take());
    drop(monitor);
    if let Ok(mut inline) = state.inline.lock() {
        let _ = inline.cancel_all();
    };
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use woof_inline::Utf16Range;

    #[derive(Default)]
    struct FakeState {
        cancelled: usize,
        deliveries: Vec<(TextScope, String, WakeHint)>,
    }

    struct FakeSession {
        metadata: FocusedElementMetadata,
        selection: Result<String, InlineError>,
        draft: Result<String, InlineError>,
        shared: Arc<Mutex<FakeState>>,
    }

    impl RuntimeInlineSession for FakeSession {
        fn metadata(&self) -> Result<FocusedElementMetadata, InlineError> {
            Ok(self.metadata.clone())
        }

        fn read(&self, scope: TextScope) -> Result<InlineRead, InlineError> {
            let text = match scope {
                TextScope::Selection => self.selection.clone()?,
                TextScope::WholeDraft => self.draft.clone()?,
            };
            Ok(InlineRead {
                scope,
                text,
                selection: self.metadata.selection,
                metadata: self.metadata.clone(),
            })
        }

        fn deliver(
            &mut self,
            expected: &InlineRead,
            replacement: &str,
            wake_hint: WakeHint,
            _focus: DeliveryFocus,
        ) -> Result<DeliveryMethod, InlineError> {
            self.shared.lock().unwrap().deliveries.push((
                expected.scope,
                replacement.to_owned(),
                wake_hint,
            ));
            Ok(DeliveryMethod::AccessibilitySelectedText)
        }

        fn cancel(&mut self) -> Result<(), InlineError> {
            self.shared.lock().unwrap().cancelled += 1;
            Ok(())
        }
    }

    struct FakeFactory {
        result: Option<Result<Box<dyn RuntimeInlineSession>, InlineError>>,
    }

    impl FocusSessionFactory for FakeFactory {
        fn capture(&mut self) -> Result<Box<dyn RuntimeInlineSession>, InlineError> {
            self.result.take().expect("fake capture called once")
        }
    }

    fn editable_metadata() -> FocusedElementMetadata {
        FocusedElementMetadata {
            bundle_id: Some("com.example.editor".into()),
            role: "AXTextArea".into(),
            frame: Some(Rect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 30.0,
            }),
            selection: Some(Utf16Range {
                location: 2,
                length: 4,
            }),
            selected_text_writable: true,
            value_writable: true,
            ..FocusedElementMetadata::default()
        }
    }

    fn fake_factory(
        metadata: FocusedElementMetadata,
        selection: Result<&str, InlineError>,
        draft: Result<&str, InlineError>,
        shared: Arc<Mutex<FakeState>>,
    ) -> FakeFactory {
        FakeFactory {
            result: Some(Ok(Box::new(FakeSession {
                metadata,
                selection: selection.map(str::to_owned),
                draft: draft.map(str::to_owned),
                shared,
            }))),
        }
    }

    #[test]
    fn rewrite_uses_nonempty_selection_and_releases_after_delivery() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata(),
            Ok("selection"),
            Ok("whole draft"),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let decision = coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap();
        assert!(matches!(
            decision,
            FocusDecision::Editable {
                scope: TextScope::Selection,
                ..
            }
        ));
        let snapshot = coordinator.prepare_rewrite(TextScope::Selection).unwrap();
        assert_eq!(snapshot.original, "selection");
        coordinator
            .deliver_rewrite(
                snapshot.session_id,
                "replacement",
                DeliveryFocus::ControllerOrTarget { controller_pid: 7 },
            )
            .unwrap();
        let state = shared.lock().unwrap();
        assert_eq!(state.cancelled, 1);
        assert_eq!(
            state.deliveries,
            vec![(
                TextScope::Selection,
                "replacement".into(),
                WakeHint::Standard
            )]
        );
    }

    #[test]
    fn session_snapshot_supports_stale_caret_event_filtering() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata(),
            Ok("selection"),
            Ok("whole draft"),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let decision = coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap();
        let session_id = match decision {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected an editable target"),
        };
        assert_eq!(
            coordinator.session_snapshot(),
            Some(InlineSessionSnapshot {
                session_id,
                status: "Selection ready"
            })
        );
        assert!(coordinator.has_session(session_id));
        assert!(!coordinator.has_session(session_id.saturating_add(1)));
        coordinator.cancel_all().unwrap();
        assert_eq!(coordinator.session_snapshot(), None);
    }

    #[test]
    fn empty_selection_falls_back_to_whole_draft() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata(),
            Ok(""),
            Ok("whole draft"),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let decision = coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap();
        assert!(matches!(
            decision,
            FocusDecision::Editable {
                scope: TextScope::WholeDraft,
                ..
            }
        ));
        coordinator.cancel_all().unwrap();
        assert_eq!(shared.lock().unwrap().cancelled, 1);
    }

    #[test]
    fn noneditable_focus_routes_without_retaining_target() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let metadata = FocusedElementMetadata {
            role: "AXButton".into(),
            ..FocusedElementMetadata::default()
        };
        let mut factory = fake_factory(
            metadata,
            Ok(""),
            Err(InlineError::TextUnavailable),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        assert_eq!(
            coordinator
                .begin_with(&mut factory, ActivationMode::Rewrite)
                .unwrap(),
            FocusDecision::NonEditable
        );
        assert_eq!(shared.lock().unwrap().cancelled, 1);
    }

    #[test]
    fn secure_focus_is_refused_without_creating_a_session() {
        let mut factory = FakeFactory {
            result: Some(Err(InlineError::SecureInput)),
        };
        let mut coordinator = InlineCoordinator::default();
        assert_eq!(
            coordinator
                .begin_with(&mut factory, ActivationMode::Rewrite)
                .unwrap_err(),
            InlineError::SecureInput
        );
        assert!(coordinator.active.is_none());
    }

    #[test]
    fn dictation_refuses_an_unreadable_activation_instead_of_assuming_empty_text() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata(),
            Err(InlineError::TextUnavailable),
            Ok("whole draft"),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        assert_eq!(
            coordinator
                .begin_with(&mut factory, ActivationMode::Dictation)
                .unwrap_err(),
            InlineError::TextUnavailable
        );
        assert!(coordinator.active.is_none());
        assert_eq!(shared.lock().unwrap().cancelled, 1);
    }

    #[test]
    fn dictation_is_delivered_once_with_gmail_wake_hint() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut metadata = editable_metadata();
        metadata.contenteditable = true;
        metadata.title = Some("Message Body".into());
        let mut factory = fake_factory(metadata, Ok(""), Ok(""), Arc::clone(&shared));
        let mut coordinator = InlineCoordinator::default();
        coordinator
            .begin_with(&mut factory, ActivationMode::Dictation)
            .unwrap();
        coordinator.stage_dictation("spoken text".into());
        let delivery = coordinator.complete_dictation().unwrap().unwrap();
        assert_eq!(delivery.receipt.domain, "mail.google.com");
        assert_eq!(delivery.transcript, "spoken text");
        assert_eq!(
            shared.lock().unwrap().deliveries,
            vec![(
                TextScope::Selection,
                "spoken text".into(),
                WakeHint::GmailContentEditable
            )]
        );
        assert!(coordinator.complete_dictation().unwrap().is_none());
    }

    #[test]
    fn modifier_release_targets_only_its_reserved_transcription() {
        let mut coordinator = InlineCoordinator::default();
        assert!(coordinator.begin_modifier_hold());
        coordinator.attach_modifier_transcription(42);
        assert_eq!(coordinator.release_modifier_hold(), Some(42));
        assert_eq!(coordinator.release_modifier_hold(), None);
        assert_eq!(coordinator.take_modifier_transcription(), Some(42));
        assert_eq!(coordinator.take_modifier_transcription(), None);
    }
}
