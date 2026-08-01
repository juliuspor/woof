use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use woof_audio::{AudioEvent, MicrophoneStopHandle};
use woof_llm::CancellationToken;

pub const MAX_TRANSCRIPTION_DURATION: Duration = Duration::from_secs(120);
pub const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;
const MAX_ITEM_BYTES: usize = 16 * 1024;
const MAX_ITEM_ID_BYTES: usize = 256;
const MAX_TRANSCRIPTION_ITEMS: usize = 256;
const LEVEL_EMIT_INTERVAL: Duration = Duration::from_millis(40);

pub trait CaptureStop: Send + Sync {
    fn stop(&self);
}

impl CaptureStop for MicrophoneStopHandle {
    fn stop(&self) {
        MicrophoneStopHandle::stop(self);
    }
}

pub type CaptureStopHandle = Arc<dyn CaptureStop>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionPhase {
    Opening,
    Listening,
    Finalizing,
    Cancelling,
}

impl SessionPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Opening => "opening",
            Self::Listening => "listening",
            Self::Finalizing => "processing",
            Self::Cancelling => "cancelling",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptionTarget {
    Companion,
    Inline,
}

impl TranscriptionTarget {
    fn from_trigger(trigger: &str) -> Result<(Self, bool), &'static str> {
        match trigger {
            "manual" | "fn_voice_chat" | "modifier_chat" => Ok((Self::Companion, false)),
            "hands_free" => Ok((Self::Companion, true)),
            "modifier_inline" => Ok((Self::Inline, false)),
            _ => Err("invalid transcription trigger"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionReservation {
    pub id: u64,
    pub cancellation: CancellationToken,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptionFailure {
    Failed,
    Overflow,
}

#[derive(Clone, PartialEq)]
pub enum TranscriptionUiEventKind {
    Start { hands_free: bool },
    Level(f32),
    Partial { item_id: String, text: String },
    ItemCompleted { item_id: String, text: String },
    Processing,
    Completed(String),
    Done,
    Cancelled,
    Failed,
    Overflow,
    Limit,
}

impl fmt::Debug for TranscriptionUiEventKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start { hands_free } => formatter
                .debug_struct("Start")
                .field("hands_free", hands_free)
                .finish(),
            Self::Level(level) => formatter.debug_tuple("Level").field(level).finish(),
            Self::Partial { item_id, text } => formatter
                .debug_struct("Partial")
                .field("item_id", item_id)
                .field("text_bytes", &text.len())
                .finish(),
            Self::ItemCompleted { item_id, text } => formatter
                .debug_struct("ItemCompleted")
                .field("item_id", item_id)
                .field("text_bytes", &text.len())
                .finish(),
            Self::Processing => formatter.write_str("Processing"),
            Self::Completed(text) => formatter
                .debug_tuple("Completed")
                .field(&format_args!("<redacted:{} bytes>", text.len()))
                .finish(),
            Self::Done => formatter.write_str("Done"),
            Self::Cancelled => formatter.write_str("Cancelled"),
            Self::Failed => formatter.write_str("Failed"),
            Self::Overflow => formatter.write_str("Overflow"),
            Self::Limit => formatter.write_str("Limit"),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct TranscriptionUiEvent {
    pub target: TranscriptionTarget,
    pub kind: TranscriptionUiEventKind,
}

impl fmt::Debug for TranscriptionUiEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TranscriptionUiEvent")
            .field("target", &self.target)
            .field("kind", &self.kind)
            .finish()
    }
}

impl TranscriptionUiEvent {
    fn new(target: TranscriptionTarget, kind: TranscriptionUiEventKind) -> Self {
        Self { target, kind }
    }
}

#[derive(Default)]
pub struct ControlEffect {
    pub stop: Option<CaptureStopHandle>,
    pub cancellation: Option<CancellationToken>,
    pub events: Vec<TranscriptionUiEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptionSnapshot {
    pub active: bool,
    pub session_id: Option<u64>,
    pub state: &'static str,
}

struct ItemState {
    text: String,
    completed: bool,
}

struct ActiveSession {
    id: u64,
    target: TranscriptionTarget,
    hands_free: bool,
    cancellation: CancellationToken,
    capture_stop: Option<CaptureStopHandle>,
    phase: SessionPhase,
    start_emitted: bool,
    processing_emitted: bool,
    terminal_emitted: bool,
    items: HashMap<String, ItemState>,
    item_order: Vec<String>,
    last_level_emit: Option<Instant>,
}

#[derive(Default)]
pub struct TranscriptionCoordinator {
    next_id: u64,
    active: Option<ActiveSession>,
}

impl TranscriptionCoordinator {
    pub fn reserve(&mut self, trigger: String) -> Result<SessionReservation, &'static str> {
        if self.active.is_some() {
            return Err("a transcription session is already active");
        }
        let (target, hands_free) = TranscriptionTarget::from_trigger(&trigger)?;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let cancellation = CancellationToken::new();
        let reservation = SessionReservation {
            id: self.next_id,
            cancellation: cancellation.clone(),
        };
        self.active = Some(ActiveSession {
            id: reservation.id,
            target,
            hands_free,
            cancellation,
            capture_stop: None,
            phase: SessionPhase::Opening,
            start_emitted: false,
            processing_emitted: false,
            terminal_emitted: false,
            items: HashMap::new(),
            item_order: Vec::new(),
            last_level_emit: None,
        });
        Ok(reservation)
    }

    pub fn attach_capture(
        &mut self,
        id: u64,
        stop: CaptureStopHandle,
    ) -> Result<bool, &'static str> {
        let active = self.active_for_mut(id)?;
        active.capture_stop = Some(stop);
        match active.phase {
            SessionPhase::Opening => {
                active.phase = SessionPhase::Listening;
                Ok(false)
            }
            SessionPhase::Finalizing | SessionPhase::Cancelling => Ok(true),
            SessionPhase::Listening => Err("microphone capture is already attached"),
        }
    }

    pub fn start_failed(&mut self, id: u64) -> Vec<TranscriptionUiEvent> {
        self.terminal_failure(id, TranscriptionFailure::Failed)
    }

    pub fn request_finalize(&mut self) -> Result<ControlEffect, &'static str> {
        let active = self
            .active
            .as_mut()
            .ok_or("no transcription session is active")?;
        match active.phase {
            SessionPhase::Cancelling => Err("transcription is being cancelled"),
            SessionPhase::Finalizing => Ok(ControlEffect::default()),
            SessionPhase::Opening => {
                active.phase = SessionPhase::Finalizing;
                Ok(ControlEffect::default())
            }
            SessionPhase::Listening => {
                active.phase = SessionPhase::Finalizing;
                let mut effect = ControlEffect {
                    stop: active.capture_stop.clone(),
                    ..ControlEffect::default()
                };
                if !active.processing_emitted {
                    active.processing_emitted = true;
                    effect.events.push(TranscriptionUiEvent::new(
                        active.target,
                        TranscriptionUiEventKind::Processing,
                    ));
                }
                Ok(effect)
            }
        }
    }

    pub fn request_cancel(&mut self) -> Result<ControlEffect, &'static str> {
        let active = self
            .active
            .as_mut()
            .ok_or("no transcription session is active")?;
        if active.phase == SessionPhase::Cancelling {
            return Ok(ControlEffect::default());
        }
        active.phase = SessionPhase::Cancelling;
        active.terminal_emitted = true;
        Ok(ControlEffect {
            stop: active.capture_stop.clone(),
            cancellation: Some(active.cancellation.clone()),
            events: vec![TranscriptionUiEvent::new(
                active.target,
                TranscriptionUiEventKind::Cancelled,
            )],
        })
    }

    pub fn request_limit(&mut self, id: u64) -> ControlEffect {
        let Ok(active) = self.active_for_mut(id) else {
            return ControlEffect::default();
        };
        if matches!(
            active.phase,
            SessionPhase::Finalizing | SessionPhase::Cancelling
        ) {
            return ControlEffect::default();
        }
        active.phase = SessionPhase::Finalizing;
        let mut effect = ControlEffect {
            stop: active.capture_stop.clone(),
            ..ControlEffect::default()
        };
        effect.events.push(TranscriptionUiEvent::new(
            active.target,
            TranscriptionUiEventKind::Limit,
        ));
        if active.start_emitted && !active.processing_emitted {
            active.processing_emitted = true;
            effect.events.push(TranscriptionUiEvent::new(
                active.target,
                TranscriptionUiEventKind::Processing,
            ));
        }
        effect
    }

    pub fn audio_event(&mut self, id: u64, event: AudioEvent) -> Vec<TranscriptionUiEvent> {
        self.audio_event_at(id, event, Instant::now())
    }

    fn audio_event_at(
        &mut self,
        id: u64,
        event: AudioEvent,
        now: Instant,
    ) -> Vec<TranscriptionUiEvent> {
        let Ok(active) = self.active_for_mut(id) else {
            return Vec::new();
        };
        if active.phase == SessionPhase::Cancelling {
            return Vec::new();
        }
        let target = active.target;
        match event {
            AudioEvent::Start => {
                if active.start_emitted {
                    return Vec::new();
                }
                active.start_emitted = true;
                let mut events = vec![TranscriptionUiEvent::new(
                    target,
                    TranscriptionUiEventKind::Start {
                        hands_free: active.hands_free,
                    },
                )];
                if active.phase == SessionPhase::Finalizing && !active.processing_emitted {
                    active.processing_emitted = true;
                    events.push(TranscriptionUiEvent::new(
                        target,
                        TranscriptionUiEventKind::Processing,
                    ));
                }
                events
            }
            AudioEvent::Level { normalized } => {
                if active.phase != SessionPhase::Listening {
                    return Vec::new();
                }
                if active
                    .last_level_emit
                    .is_some_and(|last| now.saturating_duration_since(last) < LEVEL_EMIT_INTERVAL)
                {
                    return Vec::new();
                }
                active.last_level_emit = Some(now);
                vec![TranscriptionUiEvent::new(
                    target,
                    TranscriptionUiEventKind::Level(normalized.clamp(0.0, 1.0)),
                )]
            }
            AudioEvent::Processing => {
                active.phase = SessionPhase::Finalizing;
                if active.processing_emitted {
                    Vec::new()
                } else {
                    active.processing_emitted = true;
                    vec![TranscriptionUiEvent::new(
                        target,
                        TranscriptionUiEventKind::Processing,
                    )]
                }
            }
            AudioEvent::Delta { item_id, text } => {
                if !valid_item_id(&item_id) || text.len() > MAX_ITEM_BYTES {
                    return self.overflow_active(id);
                }
                let is_new = !active.items.contains_key(&item_id);
                if is_new && active.item_order.len() >= MAX_TRANSCRIPTION_ITEMS {
                    return self.overflow_active(id);
                }
                if is_new {
                    active.item_order.push(item_id.clone());
                }
                let unchanged = active
                    .items
                    .get(&item_id)
                    .is_some_and(|item| item.text == text && !item.completed);
                if unchanged {
                    return Vec::new();
                }
                active.items.insert(
                    item_id.clone(),
                    ItemState {
                        text: text.clone(),
                        completed: false,
                    },
                );
                vec![TranscriptionUiEvent::new(
                    target,
                    TranscriptionUiEventKind::Partial { item_id, text },
                )]
            }
            AudioEvent::Done { item_id, text } => {
                if !valid_item_id(&item_id) || text.len() > MAX_ITEM_BYTES {
                    return self.overflow_active(id);
                }
                let is_new = !active.items.contains_key(&item_id);
                if is_new && active.item_order.len() >= MAX_TRANSCRIPTION_ITEMS {
                    return self.overflow_active(id);
                }
                if is_new {
                    active.item_order.push(item_id.clone());
                }
                active.items.insert(
                    item_id.clone(),
                    ItemState {
                        text: text.clone(),
                        completed: true,
                    },
                );
                vec![TranscriptionUiEvent::new(
                    target,
                    TranscriptionUiEventKind::ItemCompleted { item_id, text },
                )]
            }
            AudioEvent::Cancel => {
                active.phase = SessionPhase::Cancelling;
                active.terminal_emitted = true;
                active.cancellation.cancel();
                vec![TranscriptionUiEvent::new(
                    target,
                    TranscriptionUiEventKind::Cancelled,
                )]
            }
        }
    }

    pub fn complete(&mut self, id: u64, transcript: String) -> Vec<TranscriptionUiEvent> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        if active.id != id {
            return Vec::new();
        }
        let active = self.active.take().expect("active session was checked");
        active.cancellation.cancel();
        if active.phase == SessionPhase::Cancelling || active.terminal_emitted {
            return Vec::new();
        }
        if transcript.len() > MAX_TRANSCRIPT_BYTES {
            return vec![TranscriptionUiEvent::new(
                active.target,
                TranscriptionUiEventKind::Overflow,
            )];
        }
        let mut events = Vec::with_capacity(2);
        if !transcript.is_empty() {
            events.push(TranscriptionUiEvent::new(
                active.target,
                TranscriptionUiEventKind::Completed(transcript),
            ));
        }
        events.push(TranscriptionUiEvent::new(
            active.target,
            TranscriptionUiEventKind::Done,
        ));
        events
    }

    pub fn fail(&mut self, id: u64, failure: TranscriptionFailure) -> Vec<TranscriptionUiEvent> {
        self.terminal_failure(id, failure)
    }

    pub fn snapshot(&self) -> TranscriptionSnapshot {
        self.active.as_ref().map_or(
            TranscriptionSnapshot {
                active: false,
                session_id: None,
                state: "ready",
            },
            |active| TranscriptionSnapshot {
                active: true,
                session_id: Some(active.id),
                state: active.phase.as_str(),
            },
        )
    }

    fn overflow_active(&mut self, id: u64) -> Vec<TranscriptionUiEvent> {
        let Ok(active) = self.active_for_mut(id) else {
            return Vec::new();
        };
        if active.terminal_emitted {
            return Vec::new();
        }
        active.phase = SessionPhase::Cancelling;
        active.terminal_emitted = true;
        active.cancellation.cancel();
        vec![TranscriptionUiEvent::new(
            active.target,
            TranscriptionUiEventKind::Overflow,
        )]
    }

    fn terminal_failure(
        &mut self,
        id: u64,
        failure: TranscriptionFailure,
    ) -> Vec<TranscriptionUiEvent> {
        let Some(active) = self.active.as_ref() else {
            return Vec::new();
        };
        if active.id != id {
            return Vec::new();
        }
        let active = self.active.take().expect("active session was checked");
        active.cancellation.cancel();
        if active.phase == SessionPhase::Cancelling || active.terminal_emitted {
            return Vec::new();
        }
        let kind = match failure {
            TranscriptionFailure::Failed => TranscriptionUiEventKind::Failed,
            TranscriptionFailure::Overflow => TranscriptionUiEventKind::Overflow,
        };
        vec![TranscriptionUiEvent::new(active.target, kind)]
    }

    fn active_for_mut(&mut self, id: u64) -> Result<&mut ActiveSession, &'static str> {
        self.active
            .as_mut()
            .filter(|active| active.id == id)
            .ok_or("transcription session is no longer active")
    }
}

fn valid_item_id(item_id: &str) -> bool {
    !item_id.is_empty()
        && item_id.len() <= MAX_ITEM_ID_BYTES
        && !item_id.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    use super::*;

    #[derive(Default)]
    struct FakeStop {
        calls: AtomicUsize,
    }

    impl CaptureStop for FakeStop {
        fn stop(&self) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn event(kind: TranscriptionUiEventKind) -> TranscriptionUiEvent {
        TranscriptionUiEvent::new(TranscriptionTarget::Companion, kind)
    }

    fn attached_session() -> (TranscriptionCoordinator, SessionReservation, Arc<FakeStop>) {
        let mut coordinator = TranscriptionCoordinator::default();
        let reservation = coordinator.reserve("manual".into()).unwrap();
        let stop = Arc::new(FakeStop::default());
        assert!(!coordinator
            .attach_capture(reservation.id, stop.clone())
            .unwrap());
        (coordinator, reservation, stop)
    }

    #[test]
    fn rejects_unknown_triggers_and_overlapping_sessions() {
        let mut coordinator = TranscriptionCoordinator::default();
        assert_eq!(
            coordinator.reserve("unknown".into()).unwrap_err(),
            "invalid transcription trigger"
        );
        let first = coordinator.reserve("manual".into()).unwrap();
        assert_eq!(
            coordinator.reserve("hands_free".into()).unwrap_err(),
            "a transcription session is already active"
        );
        assert_eq!(
            coordinator.complete(first.id, "hello".into()),
            vec![
                event(TranscriptionUiEventKind::Completed("hello".into())),
                event(TranscriptionUiEventKind::Done),
            ]
        );
        assert!(coordinator.reserve("hands_free".into()).is_ok());
    }

    #[test]
    fn finalize_stops_capture_and_emits_processing_once() {
        let (mut coordinator, reservation, stop) = attached_session();
        assert_eq!(
            coordinator.audio_event(reservation.id, AudioEvent::Start),
            vec![event(TranscriptionUiEventKind::Start { hands_free: false })]
        );
        let effect = coordinator.request_finalize().unwrap();
        effect.stop.unwrap().stop();
        assert_eq!(stop.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            effect.events,
            vec![event(TranscriptionUiEventKind::Processing)]
        );
        assert!(coordinator
            .audio_event(reservation.id, AudioEvent::Processing)
            .is_empty());
        assert!(coordinator.request_finalize().unwrap().events.is_empty());
    }

    #[test]
    fn partials_replace_by_item_and_preserve_out_of_order_items() {
        let (mut coordinator, reservation, _) = attached_session();
        let _ = coordinator.audio_event(reservation.id, AudioEvent::Start);
        let partial = |item_id: &str, text: &str| AudioEvent::Delta {
            item_id: item_id.into(),
            text: text.into(),
        };
        assert_eq!(
            coordinator.audio_event(reservation.id, partial("b", "world")),
            vec![event(TranscriptionUiEventKind::Partial {
                item_id: "b".into(),
                text: "world".into(),
            })]
        );
        assert_eq!(
            coordinator.audio_event(reservation.id, partial("a", "Hel")),
            vec![event(TranscriptionUiEventKind::Partial {
                item_id: "a".into(),
                text: "Hel".into(),
            })]
        );
        assert_eq!(
            coordinator.audio_event(reservation.id, partial("a", "Hello")),
            vec![event(TranscriptionUiEventKind::Partial {
                item_id: "a".into(),
                text: "Hello".into(),
            })]
        );
        assert!(coordinator
            .audio_event(reservation.id, partial("a", "Hello"))
            .is_empty());
        assert_eq!(
            coordinator.audio_event(
                reservation.id,
                AudioEvent::Done {
                    item_id: "a".into(),
                    text: "Hallo".into(),
                },
            ),
            vec![event(TranscriptionUiEventKind::ItemCompleted {
                item_id: "a".into(),
                text: "Hallo".into(),
            })]
        );
    }

    #[test]
    fn cancellation_is_terminal_for_late_content() {
        let (mut coordinator, reservation, stop) = attached_session();
        let effect = coordinator.request_cancel().unwrap();
        effect.stop.unwrap().stop();
        effect.cancellation.unwrap().cancel();
        assert!(reservation.cancellation.is_cancelled());
        assert_eq!(stop.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            effect.events,
            vec![event(TranscriptionUiEventKind::Cancelled)]
        );
        assert!(coordinator
            .audio_event(
                reservation.id,
                AudioEvent::Done {
                    item_id: "late".into(),
                    text: "must not arrive".into(),
                },
            )
            .is_empty());
        assert!(coordinator
            .complete(reservation.id, "must not arrive".into())
            .is_empty());
    }

    #[test]
    fn duration_limit_stops_capture_and_announces_processing() {
        let (mut coordinator, reservation, stop) = attached_session();
        let _ = coordinator.audio_event(reservation.id, AudioEvent::Start);
        let effect = coordinator.request_limit(reservation.id);
        effect.stop.unwrap().stop();
        assert_eq!(stop.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            effect.events,
            vec![
                event(TranscriptionUiEventKind::Limit),
                event(TranscriptionUiEventKind::Processing),
            ]
        );
    }

    #[test]
    fn finalize_during_opening_preserves_start_before_processing() {
        let mut coordinator = TranscriptionCoordinator::default();
        let reservation = coordinator.reserve("modifier_inline".into()).unwrap();
        assert!(coordinator.request_finalize().unwrap().events.is_empty());
        let stop = Arc::new(FakeStop::default());
        assert!(coordinator.attach_capture(reservation.id, stop).unwrap());
        assert_eq!(
            coordinator.audio_event(reservation.id, AudioEvent::Start),
            vec![
                TranscriptionUiEvent::new(
                    TranscriptionTarget::Inline,
                    TranscriptionUiEventKind::Start { hands_free: false },
                ),
                TranscriptionUiEvent::new(
                    TranscriptionTarget::Inline,
                    TranscriptionUiEventKind::Processing,
                ),
            ]
        );
    }

    #[test]
    fn levels_are_coalesced_to_twenty_five_hertz() {
        let (mut coordinator, reservation, _) = attached_session();
        let _ = coordinator.audio_event(reservation.id, AudioEvent::Start);
        let start = Instant::now();
        let level = AudioEvent::Level { normalized: 0.5 };
        assert_eq!(
            coordinator.audio_event_at(reservation.id, level.clone(), start),
            vec![event(TranscriptionUiEventKind::Level(0.5))]
        );
        assert!(coordinator
            .audio_event_at(
                reservation.id,
                level.clone(),
                start + Duration::from_millis(39),
            )
            .is_empty());
        assert_eq!(
            coordinator.audio_event_at(reservation.id, level, start + Duration::from_millis(40),),
            vec![event(TranscriptionUiEventKind::Level(0.5))]
        );
    }

    #[test]
    fn oversized_content_emits_overflow_once_and_cancels() {
        let (mut coordinator, reservation, _) = attached_session();
        let events = coordinator.audio_event(
            reservation.id,
            AudioEvent::Delta {
                item_id: "item".into(),
                text: "x".repeat(MAX_ITEM_BYTES + 1),
            },
        );
        assert_eq!(events, vec![event(TranscriptionUiEventKind::Overflow)]);
        assert!(reservation.cancellation.is_cancelled());
        assert!(coordinator
            .audio_event(reservation.id, AudioEvent::Start)
            .is_empty());
        assert!(coordinator
            .fail(reservation.id, TranscriptionFailure::Overflow)
            .is_empty());
    }

    #[test]
    fn debug_output_redacts_transcript_text() {
        let event = event(TranscriptionUiEventKind::Completed("private words".into()));
        let debug = format!("{event:?}");
        assert!(!debug.contains("private words"));
        assert!(debug.contains("redacted"));
    }
}
