use tauri::{AppHandle, Manager};
use woof_inline::{
    DeliveryFocus, DeliveryMethod, FocusedElementMetadata, InlineError, InlineRead, InlineSession,
    MacOsClipboard, MacOsFocusedTarget, MacOsInputInjector, ModifierConfig, ModifierMonitor,
    PreviewWriteError, Rect, TextScope, WakeHint,
};
use woof_llm::CancellationToken;

use crate::{commands, state::UiState};

const REPLY_PROGRESS_FRAMES: [&str; 3] = ["generating.", "generating..", "generating..."];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivationMode {
    Rewrite,
    Dictation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineTask {
    Reply,
    RewriteSelection,
    RewriteDraft,
    Dictation,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FocusDecision {
    Editable {
        session_id: u64,
        frame: Option<Rect>,
        scope: TextScope,
        task: InlineTask,
        target_pid: i32,
        window_title: Option<String>,
        window_id: Option<i64>,
    },
    NonEditable,
}

pub struct RewriteSnapshot {
    pub session_id: u64,
    pub original: String,
    pub app: String,
    pub domain: String,
    pub task: InlineTask,
    pub visible_context: Option<String>,
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
    pub task: InlineTask,
    pub context_available: bool,
    pub context_reason: Option<&'static str>,
}

trait RuntimeInlineSession: Send {
    fn metadata(&self) -> Result<FocusedElementMetadata, InlineError>;
    fn read(&self, scope: TextScope) -> Result<InlineRead, InlineError>;
    fn validate(&self, expected: &InlineRead, focus: DeliveryFocus) -> Result<(), InlineError>;
    fn validate_controller_focus(&self, controller_pid: i32) -> Result<(), InlineError>;
    fn deliver(
        &mut self,
        expected: &InlineRead,
        replacement: &str,
        wake_hint: WakeHint,
        focus: DeliveryFocus,
    ) -> Result<DeliveryMethod, InlineError>;
    fn replace_whole_draft_preview(
        &mut self,
        expected_text: &str,
        replacement: &str,
        controller_pid: i32,
    ) -> Result<InlineRead, PreviewWriteError>;
    fn restore_whole_draft_preview(
        &mut self,
        expected_previews: &[&str],
        original: &InlineRead,
        controller_pid: i32,
    ) -> Result<InlineRead, InlineError>;
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

    fn validate(&self, expected: &InlineRead, focus: DeliveryFocus) -> Result<(), InlineError> {
        self.inner.validate(expected, focus)
    }

    fn validate_controller_focus(&self, controller_pid: i32) -> Result<(), InlineError> {
        self.inner.validate_controller_focus(controller_pid)
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

    fn replace_whole_draft_preview(
        &mut self,
        expected_text: &str,
        replacement: &str,
        controller_pid: i32,
    ) -> Result<InlineRead, PreviewWriteError> {
        self.inner
            .replace_whole_draft_preview(expected_text, replacement, controller_pid)
    }

    fn restore_whole_draft_preview(
        &mut self,
        expected_previews: &[&str],
        original: &InlineRead,
        controller_pid: i32,
    ) -> Result<InlineRead, InlineError> {
        self.inner
            .restore_whole_draft_preview(expected_previews, original, controller_pid)
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
    task: InlineTask,
    visible_context: Option<String>,
    context_reason: Option<&'static str>,
    cancellation: CancellationToken,
    rewrite_in_flight: bool,
    reply_progress: Option<ReplyProgress>,
    pending_transcript: Option<String>,
}

struct ReplyProgress {
    original: InlineRead,
    frame_index: usize,
    controller_pid: i32,
    owned_frames: u8,
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

        let (expected, task) = match mode {
            ActivationMode::Rewrite => {
                let selected = match session.read(TextScope::Selection) {
                    Ok(read) => read,
                    Err(error) => {
                        let _ = session.cancel();
                        return Err(error);
                    }
                };
                if !selected.text.is_empty()
                    || selected
                        .selection
                        .is_some_and(|selection| !selection.is_empty())
                {
                    (selected, InlineTask::RewriteSelection)
                } else {
                    match session.read(TextScope::WholeDraft) {
                        Ok(draft) => {
                            let task = if draft.text.trim().is_empty()
                                && is_contextual_reply_composer(&draft.metadata)
                            {
                                InlineTask::Reply
                            } else {
                                InlineTask::RewriteDraft
                            };
                            (draft, task)
                        }
                        Err(error) => {
                            let _ = session.cancel();
                            return Err(error);
                        }
                    }
                }
            }
            ActivationMode::Dictation => match session.read(TextScope::Selection) {
                Ok(read) => (read, InlineTask::Dictation),
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
        let target_pid = expected.metadata.pid;
        let window_title = expected.metadata.window_title.clone();
        let window_id = expected.metadata.window_id;
        self.active = Some(ActiveInlineSession {
            id: session_id,
            mode,
            session,
            expected,
            wake_hint,
            app,
            domain,
            task,
            visible_context: None,
            context_reason: None,
            cancellation: CancellationToken::new(),
            rewrite_in_flight: false,
            reply_progress: None,
            pending_transcript: None,
        });
        Ok(FocusDecision::Editable {
            session_id,
            frame,
            scope,
            task,
            target_pid,
            window_title,
            window_id,
        })
    }

    pub fn set_visible_context(
        &mut self,
        session_id: u64,
        context: Option<String>,
        reason: Option<&'static str>,
    ) -> bool {
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.id == session_id && active.mode == ActivationMode::Rewrite)
        else {
            return false;
        };
        active.visible_context = context.filter(|value| !value.trim().is_empty());
        active.context_reason = if active.visible_context.is_some() {
            None
        } else {
            reason
        };
        true
    }

    pub fn validate_rewrite_target(&self, session_id: u64) -> Result<(), InlineError> {
        let active = self
            .active
            .as_ref()
            .filter(|active| active.id == session_id && active.mode == ActivationMode::Rewrite)
            .ok_or(InlineError::Released)?;
        active
            .session
            .validate(&active.expected, DeliveryFocus::Target)
    }

    pub fn prepare_rewrite(&mut self, session_id: u64) -> Result<RewriteSnapshot, &'static str> {
        let active = self
            .active
            .as_mut()
            .filter(|active| active.id == session_id)
            .ok_or("the focused inline target is no longer available")?;
        if active.mode != ActivationMode::Rewrite {
            return Err("the focused inline target is being used for dictation");
        }
        if active.rewrite_in_flight {
            return Err("an inline rewrite is already in progress");
        }

        active.rewrite_in_flight = true;
        Ok(RewriteSnapshot {
            session_id: active.id,
            original: active.expected.text.clone(),
            app: active.app.clone(),
            domain: active.domain.clone(),
            task: active.task,
            visible_context: active.visible_context.clone(),
            cancellation: active.cancellation.clone(),
        })
    }

    pub fn start_reply_progress(
        &mut self,
        session_id: u64,
        controller_pid: i32,
    ) -> Result<bool, InlineError> {
        let active = self
            .active
            .as_mut()
            .filter(|active| {
                active.id == session_id
                    && active.mode == ActivationMode::Rewrite
                    && active.task == InlineTask::Reply
                    && active.rewrite_in_flight
            })
            .ok_or(InlineError::Released)?;
        if controller_pid <= 0
            || controller_pid == active.expected.metadata.pid
            || active.reply_progress.is_some()
        {
            return Err(InlineError::TargetFocusChanged);
        }

        let original = active.expected.clone();
        let frame = REPLY_PROGRESS_FRAMES[0];
        let result = active.session.replace_whole_draft_preview(
            &active.expected.text,
            frame,
            controller_pid,
        );
        match result {
            Ok(observed) if observed.scope == TextScope::WholeDraft && observed.text == frame => {
                active.expected = observed;
                active.reply_progress = Some(ReplyProgress {
                    original,
                    frame_index: 0,
                    controller_pid,
                    owned_frames: reply_progress_frame_bit(0),
                });
                Ok(true)
            }
            Ok(_) => {
                active.reply_progress = Some(ReplyProgress {
                    original,
                    frame_index: 0,
                    controller_pid,
                    owned_frames: reply_progress_frame_bit(0),
                });
                finish_reply_progress_start_failure(active, InlineError::DeliveryUnconfirmed)
            }
            Err(failure) if !failure.may_have_written => {
                if reply_progress_can_fall_back(failure.error) {
                    Ok(false)
                } else {
                    Err(failure.error)
                }
            }
            Err(failure) => {
                active.reply_progress = Some(ReplyProgress {
                    original,
                    frame_index: 0,
                    controller_pid,
                    owned_frames: reply_progress_frame_bit(0),
                });
                finish_reply_progress_start_failure(active, failure.error)
            }
        }
    }

    pub fn advance_reply_progress(&mut self, session_id: u64) -> Result<&'static str, InlineError> {
        let active = self
            .active
            .as_mut()
            .filter(|active| {
                active.id == session_id
                    && active.mode == ActivationMode::Rewrite
                    && active.task == InlineTask::Reply
                    && active.rewrite_in_flight
            })
            .ok_or(InlineError::Released)?;
        let progress = active
            .reply_progress
            .as_ref()
            .ok_or(InlineError::Released)?;
        let next_index = (progress.frame_index + 1) % REPLY_PROGRESS_FRAMES.len();
        let controller_pid = progress.controller_pid;
        let frame = REPLY_PROGRESS_FRAMES[next_index];
        let result = active.session.replace_whole_draft_preview(
            &active.expected.text,
            frame,
            controller_pid,
        );
        let observed = match result {
            Ok(observed) if observed.scope == TextScope::WholeDraft && observed.text == frame => {
                observed
            }
            Ok(_) => {
                active
                    .reply_progress
                    .as_mut()
                    .expect("reply progress was checked")
                    .owned_frames |= reply_progress_frame_bit(next_index);
                return Err(InlineError::DeliveryUnconfirmed);
            }
            Err(failure) => {
                if failure.may_have_written {
                    active
                        .reply_progress
                        .as_mut()
                        .expect("reply progress was checked")
                        .owned_frames |= reply_progress_frame_bit(next_index);
                }
                return Err(failure.error);
            }
        };
        active.expected = observed;
        active
            .reply_progress
            .as_mut()
            .expect("reply progress was checked")
            .frame_index = next_index;
        active
            .reply_progress
            .as_mut()
            .expect("reply progress was checked")
            .owned_frames = reply_progress_frame_bit(next_index);
        Ok(frame)
    }

    pub fn validate_reply_progress(&self, session_id: u64) -> Result<(), InlineError> {
        let active = self
            .active
            .as_ref()
            .filter(|active| {
                active.id == session_id
                    && active.mode == ActivationMode::Rewrite
                    && active.task == InlineTask::Reply
                    && active.rewrite_in_flight
            })
            .ok_or(InlineError::Released)?;
        let progress = active
            .reply_progress
            .as_ref()
            .ok_or(InlineError::Released)?;
        active
            .session
            .validate_controller_focus(progress.controller_pid)
    }

    pub fn rewrite_failed(&mut self, session_id: u64) -> Result<(), InlineError> {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.id != session_id)
        {
            return Ok(());
        }
        let mut active = self.active.take().expect("active session was checked");
        active.rewrite_in_flight = false;
        match restore_reply_progress(&mut active) {
            Ok(_) => {
                self.active = Some(active);
                Ok(())
            }
            Err(error) => {
                active.cancellation.cancel();
                let _ = active.session.cancel();
                Err(error)
            }
        }
    }

    pub fn cancel_rewrite_session(&mut self, session_id: u64) -> Result<bool, InlineError> {
        let matches = self.active.as_ref().is_some_and(|active| {
            active.id == session_id && active.mode == ActivationMode::Rewrite
        });
        if !matches {
            return Ok(false);
        }
        self.cancel_active()?;
        Ok(true)
    }

    pub fn generation(&self) -> u64 {
        self.next_id
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
                (ActivationMode::Rewrite, true) if active.task == InlineTask::Reply => {
                    "Drafting reply…"
                }
                (ActivationMode::Rewrite, true) => "Working on it…",
                (ActivationMode::Rewrite, false) if active.task == InlineTask::Reply => {
                    "Reading this chat…"
                }
                (ActivationMode::Rewrite, false) if active.task == InlineTask::RewriteDraft => {
                    "Draft ready"
                }
                (ActivationMode::Rewrite, false) => "Selection ready",
                (ActivationMode::Dictation, _) => "Listening…",
            },
            task: active.task,
            context_available: active.visible_context.is_some(),
            context_reason: active.context_reason,
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
        let restoration = restore_reply_progress(&mut active);
        let release = active.session.cancel();
        match (restoration, release) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(_), Ok(())) => Ok(()),
        }
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
    let restoration = match delivery.as_ref() {
        Err(error) if !delivery_may_be_uncertain(*error) => restore_reply_progress(active),
        _ => Ok(false),
    };
    let release = active.session.cancel();
    active.cancellation.cancel();
    let method = match (delivery, restoration, release) {
        (Ok(method), Ok(_), Ok(())) => method,
        (Err(_), Err(_), _) => return Err(InlineError::DeliveryUnconfirmed),
        (Err(error), Ok(_), _) | (Ok(_), Ok(_), Err(error)) => return Err(error),
        (Ok(_), Err(error), _) => return Err(error),
    };
    Ok(DeliveryReceipt {
        method,
        scope: active.expected.scope,
        app: active.app.clone(),
        domain: active.domain.clone(),
    })
}

pub(crate) fn delivery_may_be_uncertain(error: InlineError) -> bool {
    matches!(
        error,
        InlineError::Accessibility
            | InlineError::SecureInput
            | InlineError::InputInjection
            | InlineError::DeliveryUnconfirmed
            | InlineError::ClipboardRestore
            | InlineError::ClipboardChanged
    )
}

fn reply_progress_frame_bit(index: usize) -> u8 {
    1u8.checked_shl(u32::try_from(index).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

fn reply_progress_can_fall_back(error: InlineError) -> bool {
    matches!(
        error,
        InlineError::NotWritable
            | InlineError::Accessibility
            | InlineError::TextUnavailable
            | InlineError::InvalidRange
            | InlineError::DeliveryUnconfirmed
    )
}

fn finish_reply_progress_start_failure(
    active: &mut ActiveInlineSession,
    error: InlineError,
) -> Result<bool, InlineError> {
    match restore_reply_progress(active) {
        Ok(_) if reply_progress_can_fall_back(error) => Ok(false),
        Ok(_) => Err(error),
        Err(cleanup_error) => Err(cleanup_error),
    }
}

fn restore_reply_progress(active: &mut ActiveInlineSession) -> Result<bool, InlineError> {
    let Some(progress) = active.reply_progress.take() else {
        return Ok(false);
    };
    let expected_previews = REPLY_PROGRESS_FRAMES
        .iter()
        .enumerate()
        .filter_map(|(index, frame)| {
            (progress.owned_frames & reply_progress_frame_bit(index) != 0).then_some(*frame)
        })
        .collect::<Vec<_>>();
    match active.session.restore_whole_draft_preview(
        &expected_previews,
        &progress.original,
        progress.controller_pid,
    ) {
        Ok(observed) => {
            if observed.scope != TextScope::WholeDraft || observed.text != progress.original.text {
                active.reply_progress = Some(progress);
                return Err(InlineError::DeliveryUnconfirmed);
            }
            active.expected = observed;
            Ok(true)
        }
        Err(error) => {
            active.reply_progress = Some(progress);
            Err(error)
        }
    }
}

fn is_editable(metadata: &FocusedElementMetadata) -> bool {
    metadata.selected_text_writable || metadata.value_writable || metadata.contenteditable
}

fn is_contextual_reply_composer(metadata: &FocusedElementMetadata) -> bool {
    metadata.role.eq_ignore_ascii_case("AXTextArea")
        || metadata
            .subrole
            .as_deref()
            .is_some_and(|subrole| subrole.eq_ignore_ascii_case("AXTextEntryArea"))
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
    let cleanup_failed_session = state.inline.lock().ok().and_then(|mut inline| {
        let session_id = inline
            .session_snapshot()
            .map(|snapshot| snapshot.session_id);
        inline.cancel_all().err().and(session_id)
    });
    if let Some(session_id) = cleanup_failed_session {
        commands::show_terminal_inline_refusal(app, session_id, "delivery-unconfirmed");
    }
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
        delivery_expected: Vec<String>,
        previews: Vec<String>,
        restorations: Vec<String>,
        draft_override: Option<String>,
        delivery_error: Option<InlineError>,
        preview_error: Option<InlineError>,
        preview_error_writes_value: bool,
        controller_focus_error: Option<InlineError>,
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
                TextScope::WholeDraft => self
                    .shared
                    .lock()
                    .unwrap()
                    .draft_override
                    .clone()
                    .map_or_else(|| self.draft.clone(), Ok)?,
            };
            Ok(InlineRead {
                scope,
                text,
                selection: self.metadata.selection,
                metadata: self.metadata.clone(),
            })
        }

        fn validate(
            &self,
            expected: &InlineRead,
            _focus: DeliveryFocus,
        ) -> Result<(), InlineError> {
            let current = self.read(expected.scope)?;
            if current == *expected {
                Ok(())
            } else {
                Err(InlineError::TargetContentChanged)
            }
        }

        fn validate_controller_focus(&self, _controller_pid: i32) -> Result<(), InlineError> {
            self.shared
                .lock()
                .unwrap()
                .controller_focus_error
                .map_or(Ok(()), Err)
        }

        fn deliver(
            &mut self,
            expected: &InlineRead,
            replacement: &str,
            wake_hint: WakeHint,
            focus: DeliveryFocus,
        ) -> Result<DeliveryMethod, InlineError> {
            self.validate(expected, focus)?;
            let mut shared = self.shared.lock().unwrap();
            shared.delivery_expected.push(expected.text.clone());
            shared
                .deliveries
                .push((expected.scope, replacement.to_owned(), wake_hint));
            shared
                .delivery_error
                .map_or(Ok(DeliveryMethod::AccessibilitySelectedText), Err)
        }

        fn replace_whole_draft_preview(
            &mut self,
            expected_text: &str,
            replacement: &str,
            controller_pid: i32,
        ) -> Result<InlineRead, PreviewWriteError> {
            self.validate_controller_focus(controller_pid)
                .map_err(PreviewWriteError::before_write)?;
            if self
                .read(TextScope::WholeDraft)
                .map_err(PreviewWriteError::before_write)?
                .text
                != expected_text
            {
                return Err(PreviewWriteError::before_write(
                    InlineError::TargetContentChanged,
                ));
            }
            let preview_error = {
                let mut shared = self.shared.lock().unwrap();
                let error = shared.preview_error;
                if !shared.preview_error_writes_value {
                    if let Some(error) = error {
                        return Err(PreviewWriteError::before_write(error));
                    }
                }
                shared.draft_override = None;
                shared.previews.push(replacement.to_owned());
                error
            };
            self.draft = Ok(replacement.to_owned());
            let caret = self.metadata.selection.map(|_| Utf16Range {
                location: replacement.encode_utf16().count(),
                length: 0,
            });
            self.metadata.selection = caret;
            preview_error.map_or_else(
                || {
                    self.read(TextScope::WholeDraft)
                        .map_err(PreviewWriteError::after_write_started)
                },
                |error| Err(PreviewWriteError::after_write_started(error)),
            )
        }

        fn restore_whole_draft_preview(
            &mut self,
            expected_previews: &[&str],
            original: &InlineRead,
            controller_pid: i32,
        ) -> Result<InlineRead, InlineError> {
            let current = self.read(TextScope::WholeDraft)?;
            if current.text == original.text {
                return self.read(TextScope::WholeDraft);
            }
            if !expected_previews.contains(&current.text.as_str()) {
                return Err(InlineError::TargetContentChanged);
            }
            self.validate_controller_focus(controller_pid)?;
            let current = self.read(TextScope::WholeDraft)?;
            if !expected_previews.contains(&current.text.as_str()) {
                return Err(InlineError::TargetContentChanged);
            }
            {
                let mut shared = self.shared.lock().unwrap();
                shared.draft_override = None;
                shared.restorations.push(original.text.clone());
            }
            self.draft = Ok(original.text.clone());
            self.metadata.selection = original.selection;
            self.read(TextScope::WholeDraft)
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
            pid: 42,
            bundle_id: Some("com.example.editor".into()),
            window_title: Some("Roadmap — Editor".into()),
            window_id: Some(9_001),
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

    fn editable_metadata_without_selection() -> FocusedElementMetadata {
        let mut metadata = editable_metadata();
        metadata.selection = Some(Utf16Range {
            location: 0,
            length: 0,
        });
        metadata
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
                task: InlineTask::RewriteSelection,
                ..
            }
        ));
        let session_id = match decision {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected an editable target"),
        };
        let snapshot = coordinator.prepare_rewrite(session_id).unwrap();
        assert_eq!(snapshot.original, "selection");
        assert_eq!(snapshot.task, InlineTask::RewriteSelection);
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
                status: "Selection ready",
                task: InlineTask::RewriteSelection,
                context_available: false,
                context_reason: None,
            })
        );
        assert!(coordinator.has_session(session_id));
        assert!(!coordinator.has_session(session_id.saturating_add(1)));
        coordinator.cancel_all().unwrap();
        assert_eq!(coordinator.session_snapshot(), None);
    }

    #[test]
    fn stale_rewrite_actions_cannot_mutate_the_newer_session() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut first_factory = fake_factory(
            editable_metadata(),
            Ok("first"),
            Ok("first draft"),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let first_id = match coordinator
            .begin_with(&mut first_factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected an editable target"),
        };

        let mut second_factory = fake_factory(
            editable_metadata(),
            Ok("second"),
            Ok("second draft"),
            Arc::clone(&shared),
        );
        let second_id = match coordinator
            .begin_with(&mut second_factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected an editable target"),
        };

        assert!(second_id > first_id);
        assert_eq!(coordinator.generation(), second_id);
        assert!(coordinator.prepare_rewrite(first_id).is_err());
        assert!(!coordinator.cancel_rewrite_session(first_id).unwrap());
        assert!(coordinator.has_session(second_id));
        assert_eq!(shared.lock().unwrap().cancelled, 1);

        let snapshot = coordinator.prepare_rewrite(second_id).unwrap();
        assert_eq!(snapshot.original, "second");
    }

    #[test]
    fn no_selection_with_a_draft_uses_the_whole_draft() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
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
                task: InlineTask::RewriteDraft,
                ..
            }
        ));
        coordinator.cancel_all().unwrap();
        assert_eq!(shared.lock().unwrap().cancelled, 1);
    }

    #[test]
    fn empty_composer_is_a_contextual_reply() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok("  \n"),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let decision = coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap();
        let session_id = match decision {
            FocusDecision::Editable {
                session_id,
                scope: TextScope::WholeDraft,
                task: InlineTask::Reply,
                window_id: Some(9_001),
                ..
            } => session_id,
            other => panic!("expected reply target, got {other:?}"),
        };
        assert!(coordinator.set_visible_context(
            session_id,
            Some("recent visible chat".into()),
            None,
        ));
        let snapshot = coordinator.prepare_rewrite(session_id).unwrap();
        assert_eq!(snapshot.task, InlineTask::Reply);
        assert_eq!(
            snapshot.visible_context.as_deref(),
            Some("recent visible chat")
        );
    }

    #[test]
    fn contextual_reply_progress_cycles_exactly_and_final_delivery_replaces_it() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        assert!(coordinator.set_visible_context(session_id, Some("visible chat".into()), None));
        coordinator.prepare_rewrite(session_id).unwrap();

        assert_eq!(coordinator.start_reply_progress(session_id, 7), Ok(true));
        assert_eq!(
            coordinator.advance_reply_progress(session_id),
            Ok("generating..")
        );
        assert_eq!(
            coordinator.advance_reply_progress(session_id),
            Ok("generating...")
        );
        assert_eq!(
            coordinator.advance_reply_progress(session_id),
            Ok("generating.")
        );
        coordinator
            .deliver_rewrite(
                session_id,
                "Sounds good to me.",
                DeliveryFocus::ControllerOrTarget { controller_pid: 7 },
            )
            .unwrap();

        let state = shared.lock().unwrap();
        assert_eq!(
            state.previews,
            vec![
                "generating.",
                "generating..",
                "generating...",
                "generating."
            ]
        );
        assert_eq!(state.delivery_expected, vec!["generating."]);
        assert_eq!(
            state.deliveries,
            vec![(
                TextScope::WholeDraft,
                "Sounds good to me.".into(),
                WakeHint::Standard,
            )]
        );
        assert!(state.restorations.is_empty());
    }

    #[test]
    fn unsupported_reply_progress_falls_back_to_normal_reply_delivery() {
        let shared = Arc::new(Mutex::new(FakeState {
            preview_error: Some(InlineError::NotWritable),
            ..FakeState::default()
        }));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();

        assert_eq!(coordinator.start_reply_progress(session_id, 7), Ok(false));
        assert!(coordinator.has_rewrite_session(session_id));
        coordinator
            .deliver_rewrite(
                session_id,
                "The reply still works.",
                DeliveryFocus::ControllerOrTarget { controller_pid: 7 },
            )
            .unwrap();

        let state = shared.lock().unwrap();
        assert!(state.previews.is_empty());
        assert!(state.restorations.is_empty());
        assert_eq!(state.delivery_expected, vec![""]);
    }

    #[test]
    fn uncertain_initial_progress_write_is_restored_before_falling_back() {
        let shared = Arc::new(Mutex::new(FakeState {
            preview_error: Some(InlineError::DeliveryUnconfirmed),
            preview_error_writes_value: true,
            ..FakeState::default()
        }));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();

        assert_eq!(coordinator.start_reply_progress(session_id, 7), Ok(false));
        assert!(coordinator.has_rewrite_session(session_id));
        let state = shared.lock().unwrap();
        assert_eq!(state.previews, vec!["generating."]);
        assert_eq!(state.restorations, vec![""]);
    }

    #[test]
    fn uncertain_progress_advance_restores_either_attempted_exact_frame() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();
        assert_eq!(coordinator.start_reply_progress(session_id, 7), Ok(true));
        {
            let mut state = shared.lock().unwrap();
            state.preview_error = Some(InlineError::DeliveryUnconfirmed);
            state.preview_error_writes_value = true;
        }

        assert_eq!(
            coordinator.advance_reply_progress(session_id),
            Err(InlineError::DeliveryUnconfirmed)
        );
        coordinator.rewrite_failed(session_id).unwrap();

        assert!(coordinator.has_session(session_id));
        assert!(!coordinator.has_rewrite_session(session_id));
        let state = shared.lock().unwrap();
        assert_eq!(state.previews, vec!["generating.", "generating.."]);
        assert_eq!(state.restorations, vec![""]);
    }

    #[test]
    fn controller_focus_loss_leaves_the_marker_instead_of_racing_user_input() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();
        assert_eq!(coordinator.start_reply_progress(session_id, 7), Ok(true));
        shared.lock().unwrap().controller_focus_error = Some(InlineError::TargetFocusChanged);

        assert_eq!(
            coordinator.validate_reply_progress(session_id),
            Err(InlineError::TargetFocusChanged)
        );
        assert_eq!(
            coordinator.rewrite_failed(session_id),
            Err(InlineError::TargetFocusChanged)
        );

        assert!(!coordinator.has_session(session_id));
        let state = shared.lock().unwrap();
        assert!(state.restorations.is_empty());
        assert_eq!(state.previews, vec!["generating."]);
        assert_eq!(state.cancelled, 1);
    }

    #[test]
    fn failed_reply_generation_restores_only_its_exact_progress_text() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();
        coordinator.start_reply_progress(session_id, 7).unwrap();
        coordinator.advance_reply_progress(session_id).unwrap();
        coordinator.rewrite_failed(session_id).unwrap();

        assert!(coordinator.has_session(session_id));
        assert!(!coordinator.has_rewrite_session(session_id));
        assert_eq!(shared.lock().unwrap().restorations, vec![""]);
        assert_eq!(
            coordinator.prepare_rewrite(session_id).unwrap().original,
            ""
        );
    }

    #[test]
    fn user_edits_during_reply_progress_are_never_overwritten_or_cleared() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();
        coordinator.start_reply_progress(session_id, 7).unwrap();
        shared.lock().unwrap().draft_override = Some("my own message".into());

        assert_eq!(
            coordinator.advance_reply_progress(session_id),
            Err(InlineError::TargetContentChanged)
        );
        assert_eq!(
            coordinator.rewrite_failed(session_id),
            Err(InlineError::TargetContentChanged)
        );

        assert!(!coordinator.has_session(session_id));
        let state = shared.lock().unwrap();
        assert_eq!(state.draft_override.as_deref(), Some("my own message"));
        assert!(state.restorations.is_empty());
        assert_eq!(state.cancelled, 1);
    }

    #[test]
    fn a_user_typed_next_marker_is_not_claimed_by_a_failed_write_attempt() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();
        coordinator.start_reply_progress(session_id, 7).unwrap();
        shared.lock().unwrap().draft_override = Some("generating..".into());

        assert_eq!(
            coordinator.advance_reply_progress(session_id),
            Err(InlineError::TargetContentChanged)
        );
        assert_eq!(
            coordinator.rewrite_failed(session_id),
            Err(InlineError::TargetContentChanged)
        );

        let state = shared.lock().unwrap();
        assert_eq!(state.draft_override.as_deref(), Some("generating.."));
        assert!(state.restorations.is_empty());
    }

    #[test]
    fn cancelling_reply_progress_restores_the_empty_composer_before_release() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();
        coordinator.start_reply_progress(session_id, 7).unwrap();

        assert!(coordinator.cancel_rewrite_session(session_id).unwrap());
        assert!(!coordinator.has_session(session_id));
        let state = shared.lock().unwrap();
        assert_eq!(state.restorations, vec![""]);
        assert_eq!(state.cancelled, 1);
    }

    #[test]
    fn cancellation_reports_when_focus_loss_makes_marker_cleanup_unsafe() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();
        coordinator.start_reply_progress(session_id, 7).unwrap();
        shared.lock().unwrap().controller_focus_error = Some(InlineError::TargetFocusChanged);

        assert_eq!(
            coordinator.cancel_rewrite_session(session_id),
            Err(InlineError::TargetFocusChanged)
        );
        assert!(!coordinator.has_session(session_id));
        let state = shared.lock().unwrap();
        assert!(state.restorations.is_empty());
        assert_eq!(state.previews, vec!["generating."]);
        assert_eq!(state.cancelled, 1);
    }

    #[test]
    fn uncertain_final_delivery_never_erases_a_reply_that_may_have_landed() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata_without_selection(),
            Ok(""),
            Ok(""),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        let session_id = match coordinator
            .begin_with(&mut factory, ActivationMode::Rewrite)
            .unwrap()
        {
            FocusDecision::Editable { session_id, .. } => session_id,
            FocusDecision::NonEditable => panic!("expected a reply target"),
        };
        coordinator.prepare_rewrite(session_id).unwrap();
        coordinator.start_reply_progress(session_id, 7).unwrap();
        shared.lock().unwrap().delivery_error = Some(InlineError::DeliveryUnconfirmed);

        assert!(matches!(
            coordinator.deliver_rewrite(
                session_id,
                "Possible final reply",
                DeliveryFocus::ControllerOrTarget { controller_pid: 7 },
            ),
            Err(InlineError::DeliveryUnconfirmed)
        ));
        let state = shared.lock().unwrap();
        assert!(state.restorations.is_empty());
        assert_eq!(state.cancelled, 1);
    }

    #[test]
    fn post_input_and_confirmation_failures_are_classified_as_uncertain() {
        for error in [
            InlineError::Accessibility,
            InlineError::SecureInput,
            InlineError::InputInjection,
            InlineError::DeliveryUnconfirmed,
            InlineError::ClipboardRestore,
            InlineError::ClipboardChanged,
        ] {
            assert!(delivery_may_be_uncertain(error), "{error:?}");
        }
        assert!(!delivery_may_be_uncertain(InlineError::ClipboardWrite));
        assert!(!delivery_may_be_uncertain(
            InlineError::TargetContentChanged
        ));
    }

    #[test]
    fn empty_search_or_plain_text_fields_are_not_contextual_replies() {
        for role in ["AXSearchField", "AXTextField", "AXComboBox"] {
            let shared = Arc::new(Mutex::new(FakeState::default()));
            let mut metadata = editable_metadata_without_selection();
            metadata.role = role.to_string();
            let mut factory = fake_factory(metadata, Ok(""), Ok(""), Arc::clone(&shared));
            let mut coordinator = InlineCoordinator::default();
            assert!(matches!(
                coordinator
                    .begin_with(&mut factory, ActivationMode::Rewrite)
                    .unwrap(),
                FocusDecision::Editable {
                    scope: TextScope::WholeDraft,
                    task: InlineTask::RewriteDraft,
                    ..
                }
            ));
        }
    }

    #[test]
    fn empty_ax_text_entry_area_is_a_contextual_reply_candidate() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut metadata = editable_metadata_without_selection();
        metadata.role = "AXGroup".to_string();
        metadata.subrole = Some("AXTextEntryArea".to_string());
        metadata.contenteditable = true;
        let mut factory = fake_factory(metadata, Ok(""), Ok(""), Arc::clone(&shared));
        let mut coordinator = InlineCoordinator::default();
        assert!(matches!(
            coordinator
                .begin_with(&mut factory, ActivationMode::Rewrite)
                .unwrap(),
            FocusDecision::Editable {
                scope: TextScope::WholeDraft,
                task: InlineTask::Reply,
                ..
            }
        ));
    }

    #[test]
    fn a_nonempty_selection_range_stays_authoritative_for_whitespace() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut factory = fake_factory(
            editable_metadata(),
            Ok("   "),
            Ok("whole draft"),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        assert!(matches!(
            coordinator
                .begin_with(&mut factory, ActivationMode::Rewrite)
                .unwrap(),
            FocusDecision::Editable {
                scope: TextScope::Selection,
                task: InlineTask::RewriteSelection,
                ..
            }
        ));
    }

    #[test]
    fn selected_text_stays_authoritative_when_the_range_is_unavailable() {
        let shared = Arc::new(Mutex::new(FakeState::default()));
        let mut metadata = editable_metadata();
        metadata.selection = None;
        let mut factory = fake_factory(
            metadata,
            Ok("selection"),
            Ok("whole draft"),
            Arc::clone(&shared),
        );
        let mut coordinator = InlineCoordinator::default();
        assert!(matches!(
            coordinator
                .begin_with(&mut factory, ActivationMode::Rewrite)
                .unwrap(),
            FocusDecision::Editable {
                scope: TextScope::Selection,
                task: InlineTask::RewriteSelection,
                ..
            }
        ));
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
