use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use tokio::sync::{mpsc, Notify};
use woof_llm::{
    ApiKey, AudioCommand, CancellationToken, RealtimeError, RealtimeSessionConfig,
    RealtimeTranscriptionClient, TranscriptionEvent,
};

use crate::{AudioError, AudioSource};

const AUDIO_COMMAND_CAPACITY: usize = 32;
// woof-llm accepts at most 128 transcript items. A capacity of 256 therefore
// leaves room for every terminal item while partial revisions are coalesced.
const BACKEND_EVENT_CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub enum AudioEvent {
    Start,
    Level {
        normalized: f32,
    },
    Processing,
    /// Latest reconciled partial for `item_id`, not an unassembled token.
    Delta {
        item_id: String,
        text: String,
    },
    Done {
        item_id: String,
        text: String,
    },
    Cancel,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranscriptionOutcome {
    pub transcript: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendEventDisposition {
    Queued,
    Coalesced,
    Superseded,
    IgnoredStatus,
    Overflow,
}

#[derive(Clone)]
pub struct BackendEventSender {
    shared: Arc<BackendEventShared>,
}

impl BackendEventSender {
    /// Publishes without blocking the Realtime socket task.
    ///
    /// Status events are redundant with the capture loop and are discarded.
    /// Revisions and finals for a pending item replace the older revision in
    /// place. A new final item may evict another partial,
    /// but never another final item. If a backend violates the bounded item
    /// contract and fills the queue entirely with finals, the receiver observes
    /// an explicit overflow instead of returning a successful transcript with
    /// a missing terminal event.
    pub fn send(&self, event: TranscriptionEvent) -> BackendEventDisposition {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let disposition = state.push(event);
        drop(state);
        if disposition != BackendEventDisposition::IgnoredStatus {
            self.shared.notify.notify_one();
        }
        disposition
    }
}

struct BackendEventReceiver {
    shared: Arc<BackendEventShared>,
}

impl BackendEventReceiver {
    async fn recv(&self) -> Result<TranscriptionEvent, AudioError> {
        loop {
            let notified = self.shared.notify.notified();
            if let Some(event) = self.try_recv()? {
                return Ok(event);
            }
            notified.await;
        }
    }

    fn try_recv(&self) -> Result<Option<TranscriptionEvent>, AudioError> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.overflowed {
            return Err(AudioError::BufferOverflow);
        }
        Ok(state.events.pop_front())
    }
}

struct BackendEventShared {
    state: Mutex<BackendEventState>,
    notify: Notify,
}

struct BackendEventState {
    events: VecDeque<TranscriptionEvent>,
    capacity: usize,
    overflowed: bool,
}

impl BackendEventState {
    fn push(&mut self, event: TranscriptionEvent) -> BackendEventDisposition {
        match event {
            TranscriptionEvent::Started
            | TranscriptionEvent::Level { .. }
            | TranscriptionEvent::Processing => BackendEventDisposition::IgnoredStatus,
            TranscriptionEvent::Partial { item_id, text } => {
                if self.events.iter().any(
                    |event| matches!(event, TranscriptionEvent::Done { item_id: done_id, .. } if done_id == &item_id),
                ) {
                    return BackendEventDisposition::Superseded;
                }
                if let Some(TranscriptionEvent::Partial {
                    text: pending_text, ..
                }) = self.events.iter_mut().find(|event| {
                    matches!(event, TranscriptionEvent::Partial { item_id: pending_id, .. } if pending_id == &item_id)
                }) {
                    *pending_text = text;
                    return BackendEventDisposition::Coalesced;
                }
                self.push_content(TranscriptionEvent::Partial { item_id, text }, false)
            }
            TranscriptionEvent::Done { item_id, text } => {
                if let Some(index) = self.events.iter().position(|event| {
                    matches!(event, TranscriptionEvent::Partial { item_id: pending_id, .. } if pending_id == &item_id)
                }) {
                    self.events[index] = TranscriptionEvent::Done { item_id, text };
                    return BackendEventDisposition::Coalesced;
                }
                if let Some(TranscriptionEvent::Done {
                    text: final_text, ..
                }) = self.events.iter_mut().find(|event| {
                    matches!(event, TranscriptionEvent::Done { item_id: done_id, .. } if done_id == &item_id)
                }) {
                    *final_text = text;
                    return BackendEventDisposition::Coalesced;
                }
                self.push_content(TranscriptionEvent::Done { item_id, text }, true)
            }
        }
    }

    fn push_content(
        &mut self,
        event: TranscriptionEvent,
        terminal: bool,
    ) -> BackendEventDisposition {
        if self.events.len() >= self.capacity {
            if let Some(index) = self
                .events
                .iter()
                .position(|event| matches!(event, TranscriptionEvent::Partial { .. }))
            {
                self.events.remove(index);
            } else if !terminal {
                return BackendEventDisposition::Superseded;
            } else {
                self.overflowed = true;
                return BackendEventDisposition::Overflow;
            }
        }
        self.events.push_back(event);
        BackendEventDisposition::Queued
    }
}

fn backend_event_channel(capacity: usize) -> (BackendEventSender, BackendEventReceiver) {
    let shared = Arc::new(BackendEventShared {
        state: Mutex::new(BackendEventState {
            events: VecDeque::with_capacity(capacity),
            capacity,
            overflowed: false,
        }),
        notify: Notify::new(),
    });
    (
        BackendEventSender {
            shared: Arc::clone(&shared),
        },
        BackendEventReceiver { shared },
    )
}

#[async_trait]
pub trait RealtimeBackend: Send + Sync {
    async fn run(
        &self,
        key: &ApiKey,
        config: &RealtimeSessionConfig,
        audio: mpsc::Receiver<AudioCommand>,
        cancellation: &CancellationToken,
        events: BackendEventSender,
    ) -> Result<String, RealtimeError>;
}

#[derive(Clone, Debug, Default)]
pub struct OpenAiRealtimeBackend {
    client: RealtimeTranscriptionClient,
}

#[async_trait]
impl RealtimeBackend for OpenAiRealtimeBackend {
    async fn run(
        &self,
        key: &ApiKey,
        config: &RealtimeSessionConfig,
        audio: mpsc::Receiver<AudioCommand>,
        cancellation: &CancellationToken,
        events: BackendEventSender,
    ) -> Result<String, RealtimeError> {
        self.client
            .run(key, config, audio, cancellation, move |event| {
                events.send(event);
            })
            .await
    }
}

#[derive(Clone, Debug)]
pub struct TranscriptionSession<B = OpenAiRealtimeBackend> {
    backend: B,
}

impl Default for TranscriptionSession<OpenAiRealtimeBackend> {
    fn default() -> Self {
        Self::new(OpenAiRealtimeBackend::default())
    }
}

impl<B> TranscriptionSession<B>
where
    B: RealtimeBackend,
{
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    /// Captures until the source ends, then commits and waits for transcription.
    ///
    /// Cancellation drops the in-flight microphone and WebSocket work without
    /// committing and emits exactly one [`AudioEvent::Cancel`]. The API key is
    /// borrowed for the duration of this call and is never cloned or persisted.
    pub async fn run<S, F>(
        &self,
        source: &mut S,
        key: &ApiKey,
        config: &RealtimeSessionConfig,
        cancellation: &CancellationToken,
        mut callback: F,
    ) -> Result<TranscriptionOutcome, AudioError>
    where
        S: AudioSource,
        F: FnMut(AudioEvent) + Send,
    {
        callback(AudioEvent::Start);

        let (command_sender, command_receiver) = mpsc::channel(AUDIO_COMMAND_CAPACITY);
        let (event_sender, event_receiver) = backend_event_channel(BACKEND_EVENT_CAPACITY);
        let backend = self
            .backend
            .run(key, config, command_receiver, cancellation, event_sender);
        tokio::pin!(backend);

        let mut source_finished = false;
        let mut sent_audio = false;
        let mut transcript_state = EventTranscriptState::default();

        let backend_result = loop {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    source.stop();
                    callback(AudioEvent::Cancel);
                    return Err(AudioError::Cancelled);
                }
                result = &mut backend => {
                    break result;
                }
                event = event_receiver.recv() => {
                    let event = event?;
                    publish_backend_event(event, &mut transcript_state, &mut callback);
                }
                frame = source.next_frame(cancellation), if !source_finished => {
                    match frame {
                        Ok(Some(frame)) if frame.is_empty() => {}
                        Ok(Some(frame)) => {
                            let level = frame.normalized_level();
                            if let Err(error) = send_command(
                                &command_sender,
                                AudioCommand::AppendPcm16(frame.into_samples()),
                                cancellation,
                            )
                            .await
                            {
                                source.stop();
                                if matches!(error, AudioError::Cancelled) {
                                    callback(AudioEvent::Cancel);
                                }
                                return Err(error);
                            }
                            sent_audio = true;
                            callback(AudioEvent::Level { normalized: level });
                        }
                        Ok(None) => {
                            source_finished = true;
                            if let Err(error) = send_command(
                                &command_sender,
                                AudioCommand::Finish,
                                cancellation,
                            )
                            .await
                            {
                                source.stop();
                                if matches!(error, AudioError::Cancelled) {
                                    callback(AudioEvent::Cancel);
                                }
                                return Err(error);
                            }
                            if sent_audio {
                                callback(AudioEvent::Processing);
                            }
                        }
                        Err(AudioError::Cancelled) => {
                            source.stop();
                            callback(AudioEvent::Cancel);
                            return Err(AudioError::Cancelled);
                        }
                        Err(error) => {
                            source.stop();
                            return Err(error);
                        }
                    }
                }
            }
        };

        source.stop();
        while let Some(event) = event_receiver.try_recv()? {
            publish_backend_event(event, &mut transcript_state, &mut callback);
        }

        match backend_result {
            Ok(transcript) => Ok(TranscriptionOutcome { transcript }),
            Err(RealtimeError::Cancelled) => {
                callback(AudioEvent::Cancel);
                Err(AudioError::Cancelled)
            }
            Err(error) => Err(AudioError::Realtime(error)),
        }
    }
}

async fn send_command(
    sender: &mpsc::Sender<AudioCommand>,
    command: AudioCommand,
    cancellation: &CancellationToken,
) -> Result<(), AudioError> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(AudioError::Cancelled),
        sent = sender.send(command) => sent.map_err(|_| AudioError::SessionClosed),
    }
}

fn publish_backend_event<F>(
    event: TranscriptionEvent,
    state: &mut EventTranscriptState,
    callback: &mut F,
) where
    F: FnMut(AudioEvent),
{
    match event {
        TranscriptionEvent::Partial { item_id, text } => {
            if let Some(text) = state.replace_partial(&item_id, &text) {
                callback(AudioEvent::Delta { item_id, text });
            }
        }
        TranscriptionEvent::Done { item_id, text } => {
            let text = state.complete(&item_id, &text);
            callback(AudioEvent::Done { item_id, text });
        }
        TranscriptionEvent::Started
        | TranscriptionEvent::Level { .. }
        | TranscriptionEvent::Processing => {}
    }
}

#[derive(Debug, Default)]
struct EventTranscriptState {
    partials: HashMap<String, String>,
    finals: HashSet<String>,
}

impl EventTranscriptState {
    fn replace_partial(&mut self, item_id: &str, text: &str) -> Option<String> {
        if self.finals.contains(item_id) {
            return None;
        }
        self.partials.insert(item_id.to_owned(), text.to_owned());
        Some(text.to_owned())
    }

    fn complete(&mut self, item_id: &str, text: &str) -> String {
        self.partials.remove(item_id);
        self.finals.insert(item_id.to_owned());
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    use super::*;
    use crate::AudioFrame;

    struct FakeSource {
        frames: VecDeque<AudioFrame>,
    }

    #[async_trait]
    impl AudioSource for FakeSource {
        async fn next_frame(
            &mut self,
            cancellation: &CancellationToken,
        ) -> Result<Option<AudioFrame>, AudioError> {
            if cancellation.is_cancelled() {
                return Err(AudioError::Cancelled);
            }
            Ok(self.frames.pop_front())
        }
    }

    #[derive(Clone, Default)]
    struct FakeBackend {
        commands: Arc<Mutex<Vec<AudioCommand>>>,
    }

    #[async_trait]
    impl RealtimeBackend for FakeBackend {
        async fn run(
            &self,
            _key: &ApiKey,
            _config: &RealtimeSessionConfig,
            mut audio: mpsc::Receiver<AudioCommand>,
            _cancellation: &CancellationToken,
            events: BackendEventSender,
        ) -> Result<String, RealtimeError> {
            let _ = events.send(TranscriptionEvent::Started);
            while let Some(command) = audio.recv().await {
                self.commands.lock().unwrap().push(command.clone());
                if command == AudioCommand::Finish {
                    let _ = events.send(TranscriptionEvent::Processing);
                    let _ = events.send(TranscriptionEvent::Partial {
                        item_id: "item-a".into(),
                        text: "Helo".into(),
                    });
                    let _ = events.send(TranscriptionEvent::Partial {
                        item_id: "item-b".into(),
                        text: "dog".into(),
                    });
                    let _ = events.send(TranscriptionEvent::Partial {
                        item_id: "item-a".into(),
                        text: "Hello".into(),
                    });
                    let _ = events.send(TranscriptionEvent::Done {
                        item_id: "item-b".into(),
                        text: "dog.".into(),
                    });
                    let _ = events.send(TranscriptionEvent::Done {
                        item_id: "item-a".into(),
                        text: "Hello".into(),
                    });
                    let _ = events.send(TranscriptionEvent::Partial {
                        item_id: "item-a".into(),
                        text: "late".into(),
                    });
                    return Ok("Hello dog.".into());
                }
            }
            Err(RealtimeError::AudioClosed)
        }
    }

    #[tokio::test]
    async fn streams_frames_and_preserves_reconciled_finals_under_coalescing() {
        let backend = FakeBackend::default();
        let commands = Arc::clone(&backend.commands);
        let session = TranscriptionSession::new(backend);
        let mut source = FakeSource {
            frames: VecDeque::from([AudioFrame::new(vec![0, i16::MAX])]),
        };
        let key = ApiKey::new("sk-test-only").unwrap();
        let cancellation = CancellationToken::new();
        let mut events = Vec::new();

        let outcome = session
            .run(
                &mut source,
                &key,
                &RealtimeSessionConfig::default(),
                &cancellation,
                |event| events.push(event),
            )
            .await
            .unwrap();

        assert_eq!(outcome.transcript, "Hello dog.");
        assert_eq!(
            *commands.lock().unwrap(),
            vec![
                AudioCommand::AppendPcm16(vec![0, i16::MAX]),
                AudioCommand::Finish
            ]
        );
        assert_eq!(events[0], AudioEvent::Start);
        assert!(matches!(events[1], AudioEvent::Level { .. }));
        assert_eq!(events[2], AudioEvent::Processing);
        assert_eq!(
            events[3..],
            [
                AudioEvent::Done {
                    item_id: "item-a".into(),
                    text: "Hello".into()
                },
                AudioEvent::Done {
                    item_id: "item-b".into(),
                    text: "dog.".into()
                }
            ]
        );
    }

    struct PendingSource;

    #[async_trait]
    impl AudioSource for PendingSource {
        async fn next_frame(
            &mut self,
            cancellation: &CancellationToken,
        ) -> Result<Option<AudioFrame>, AudioError> {
            cancellation.cancelled().await;
            Err(AudioError::Cancelled)
        }
    }

    #[derive(Clone, Copy)]
    struct PendingBackend;

    #[async_trait]
    impl RealtimeBackend for PendingBackend {
        async fn run(
            &self,
            _key: &ApiKey,
            _config: &RealtimeSessionConfig,
            _audio: mpsc::Receiver<AudioCommand>,
            cancellation: &CancellationToken,
            _events: BackendEventSender,
        ) -> Result<String, RealtimeError> {
            cancellation.cancelled().await;
            Err(RealtimeError::Cancelled)
        }
    }

    #[tokio::test]
    async fn cancellation_is_terminal_and_emitted_once() {
        let session = TranscriptionSession::new(PendingBackend);
        let mut source = PendingSource;
        let key = ApiKey::new("sk-test-only").unwrap();
        let cancellation = CancellationToken::new();
        let canceller = cancellation.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            canceller.cancel();
        });
        let mut events = Vec::new();

        let result = session
            .run(
                &mut source,
                &key,
                &RealtimeSessionConfig::default(),
                &cancellation,
                |event| events.push(event),
            )
            .await;

        assert!(matches!(result, Err(AudioError::Cancelled)));
        assert_eq!(events, vec![AudioEvent::Start, AudioEvent::Cancel]);
    }

    #[test]
    fn bounded_backend_events_coalesce_partials_and_preserve_finals() {
        let (sender, receiver) = backend_event_channel(2);
        assert_eq!(
            sender.send(TranscriptionEvent::Partial {
                item_id: "item-a".into(),
                text: "old".into(),
            }),
            BackendEventDisposition::Queued
        );
        assert_eq!(
            sender.send(TranscriptionEvent::Partial {
                item_id: "item-a".into(),
                text: "corrected".into(),
            }),
            BackendEventDisposition::Coalesced
        );
        assert_eq!(
            sender.send(TranscriptionEvent::Partial {
                item_id: "item-b".into(),
                text: "pending".into(),
            }),
            BackendEventDisposition::Queued
        );
        assert_eq!(
            sender.send(TranscriptionEvent::Done {
                item_id: "item-a".into(),
                text: "final-a".into(),
            }),
            BackendEventDisposition::Coalesced
        );
        assert_eq!(
            sender.send(TranscriptionEvent::Done {
                item_id: "item-b".into(),
                text: "final-b".into(),
            }),
            BackendEventDisposition::Coalesced
        );

        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(TranscriptionEvent::Done {
                item_id: "item-a".into(),
                text: "final-a".into(),
            })
        );
        assert_eq!(
            receiver.try_recv().unwrap(),
            Some(TranscriptionEvent::Done {
                item_id: "item-b".into(),
                text: "final-b".into(),
            })
        );
        assert_eq!(receiver.try_recv().unwrap(), None);
    }

    #[test]
    fn terminal_saturation_becomes_an_explicit_overflow() {
        let (sender, receiver) = backend_event_channel(2);
        for item_id in ["item-a", "item-b"] {
            assert_eq!(
                sender.send(TranscriptionEvent::Done {
                    item_id: item_id.into(),
                    text: "final".into(),
                }),
                BackendEventDisposition::Queued
            );
        }
        assert_eq!(
            sender.send(TranscriptionEvent::Done {
                item_id: "item-c".into(),
                text: "final".into(),
            }),
            BackendEventDisposition::Overflow
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(AudioError::BufferOverflow)
        ));
    }

    #[test]
    fn redundant_backend_status_does_not_consume_capacity() {
        let (sender, receiver) = backend_event_channel(1);
        for event in [
            TranscriptionEvent::Started,
            TranscriptionEvent::Level { normalized: 0.5 },
            TranscriptionEvent::Processing,
        ] {
            assert_eq!(sender.send(event), BackendEventDisposition::IgnoredStatus);
        }
        assert_eq!(receiver.try_recv().unwrap(), None);
    }
}
