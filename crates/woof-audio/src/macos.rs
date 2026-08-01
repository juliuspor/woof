use std::{
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use async_trait::async_trait;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, SampleFormat, SizedSample, Stream, StreamConfig,
};
use tokio::sync::{mpsc, Notify};
use woof_llm::CancellationToken;

use crate::{
    request_microphone_authorization_with_cancellation, AudioError, AudioFrame, AudioSource,
    MicrophoneAuthorization, Pcm16Resampler,
};

const FRAME_BUFFER_CAPACITY: usize = 64;
const STREAM_STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const FAILURE_NONE: u8 = 0;
const FAILURE_STREAM: u8 = 1;
const FAILURE_OVERFLOW: u8 = 2;

#[derive(Clone, Debug)]
pub struct MicrophoneStopHandle {
    state: Arc<MicrophoneState>,
}

impl MicrophoneStopHandle {
    /// Gracefully stops capture. The session commits all already-buffered PCM.
    pub fn stop(&self) {
        self.state.stop();
    }
}

pub struct MacOsMicrophone {
    frames: mpsc::Receiver<AudioFrame>,
    state: Arc<MicrophoneState>,
    worker: Option<JoinHandle<()>>,
}

impl MacOsMicrophone {
    pub async fn open() -> Result<(Self, MicrophoneStopHandle), AudioError> {
        Self::open_with_cancellation(&CancellationToken::new()).await
    }

    /// Opens the default microphone with bounded startup and caller cancellation.
    pub async fn open_with_cancellation(
        cancellation: &CancellationToken,
    ) -> Result<(Self, MicrophoneStopHandle), AudioError> {
        match request_microphone_authorization_with_cancellation(cancellation).await? {
            MicrophoneAuthorization::Authorized => {}
            MicrophoneAuthorization::Restricted => {
                return Err(AudioError::PermissionRestricted);
            }
            MicrophoneAuthorization::Denied => return Err(AudioError::PermissionDenied),
            MicrophoneAuthorization::NotDetermined => {
                return Err(AudioError::PermissionRequest);
            }
        }

        let state = Arc::new(MicrophoneState::default());
        let (sender, frames) = mpsc::channel(FRAME_BUFFER_CAPACITY);
        let (startup_sender, startup_receiver) = tokio::sync::oneshot::channel();
        let worker_state = Arc::clone(&state);
        let worker = thread::Builder::new()
            .name("woof-microphone".into())
            .spawn(move || capture_thread(sender, worker_state, startup_sender))
            .map_err(|_| AudioError::StreamConfiguration)?;
        match wait_for_startup(startup_receiver, cancellation, STREAM_STARTUP_TIMEOUT).await {
            Ok(()) => {}
            Err(StartupWaitError::Capture(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(StartupWaitError::Closed) => {
                let _ = worker.join();
                return Err(AudioError::Stream);
            }
            Err(StartupWaitError::Cancelled) => {
                state.stop();
                // Detach instead of blocking cancellation on an operating-system
                // device call. A late startup observes the closed oneshot and
                // immediately drops its stream.
                drop(worker);
                return Err(AudioError::Cancelled);
            }
            Err(StartupWaitError::TimedOut) => {
                state.stop();
                // The receiver is gone, so a worker that eventually returns from
                // device setup tears the stream down without publishing audio.
                drop(worker);
                return Err(AudioError::StreamStartupTimeout);
            }
        }

        let stop_handle = MicrophoneStopHandle {
            state: Arc::clone(&state),
        };
        Ok((
            Self {
                frames,
                state,
                worker: Some(worker),
            },
            stop_handle,
        ))
    }

    fn join_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[async_trait]
impl AudioSource for MacOsMicrophone {
    async fn next_frame(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<AudioFrame>, AudioError> {
        loop {
            if cancellation.is_cancelled() {
                return Err(AudioError::Cancelled);
            }
            match self.state.failure.load(Ordering::SeqCst) {
                FAILURE_STREAM => {
                    self.join_worker();
                    return Err(AudioError::Stream);
                }
                FAILURE_OVERFLOW => {
                    self.join_worker();
                    return Err(AudioError::BufferOverflow);
                }
                _ => {}
            }
            if self.state.stopped.load(Ordering::SeqCst) {
                self.join_worker();
                match self.state.failure.load(Ordering::SeqCst) {
                    FAILURE_STREAM => return Err(AudioError::Stream),
                    FAILURE_OVERFLOW => return Err(AudioError::BufferOverflow),
                    _ => {}
                }
                return match self.frames.try_recv() {
                    Ok(frame) => Ok(Some(frame)),
                    Err(
                        mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected,
                    ) => Ok(None),
                };
            }

            let notified = self.state.notify.notified();
            if self.state.has_signal() {
                continue;
            }
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(AudioError::Cancelled),
                _ = notified => {}
                frame = self.frames.recv() => {
                    return frame.map(Some).ok_or(AudioError::Stream);
                }
            }
        }
    }

    fn stop(&mut self) {
        self.state.stop();
        self.join_worker();
    }
}

#[derive(Debug)]
enum StartupWaitError {
    Capture(AudioError),
    Cancelled,
    TimedOut,
    Closed,
}

async fn wait_for_startup(
    receiver: tokio::sync::oneshot::Receiver<Result<(), AudioError>>,
    cancellation: &CancellationToken,
    timeout: Duration,
) -> Result<(), StartupWaitError> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(StartupWaitError::Cancelled),
        result = tokio::time::timeout(timeout, receiver) => match result {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(StartupWaitError::Capture(error)),
            Ok(Err(_)) => Err(StartupWaitError::Closed),
            Err(_) => Err(StartupWaitError::TimedOut),
        },
    }
}

impl Drop for MacOsMicrophone {
    fn drop(&mut self) {
        self.state.stop();
        self.join_worker();
    }
}

#[derive(Debug, Default)]
struct MicrophoneState {
    stopped: AtomicBool,
    failure: AtomicU8,
    notify: Notify,
    wait_lock: Mutex<()>,
    wait: Condvar,
}

impl MicrophoneState {
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.wake();
    }

    fn fail(&self, failure: u8) {
        let _ = self.failure.compare_exchange(
            FAILURE_NONE,
            failure,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.wake();
    }

    fn wake(&self) {
        self.notify.notify_waiters();
        self.wait.notify_all();
    }

    fn has_signal(&self) -> bool {
        self.stopped.load(Ordering::SeqCst) || self.failure.load(Ordering::SeqCst) != FAILURE_NONE
    }

    fn wait_until_signal(&self) {
        let mut guard = self.wait_lock.lock().unwrap();
        while !self.has_signal() {
            guard = self.wait.wait(guard).unwrap();
        }
    }
}

fn capture_thread(
    sender: mpsc::Sender<AudioFrame>,
    state: Arc<MicrophoneState>,
    startup: tokio::sync::oneshot::Sender<Result<(), AudioError>>,
) {
    let stream = open_stream(sender, Arc::clone(&state));
    match stream {
        Ok(stream) => {
            if startup.send(Ok(())).is_err() {
                return;
            }
            state.wait_until_signal();
            drop(stream);
        }
        Err(error) => {
            let _ = startup.send(Err(error));
        }
    }
}

fn open_stream(
    sender: mpsc::Sender<AudioFrame>,
    state: Arc<MicrophoneState>,
) -> Result<Stream, AudioError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(AudioError::DeviceUnavailable)?;
    let supported = device
        .default_input_config()
        .map_err(|_| AudioError::StreamConfiguration)?;
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::I8 => build_stream::<i8>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::I32 => build_stream::<i32>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::I64 => build_stream::<i64>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::U8 => build_stream::<u8>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::U32 => build_stream::<u32>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::U64 => build_stream::<u64>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::F32 => build_stream::<f32>(&device, &config, sender, Arc::clone(&state)),
        SampleFormat::F64 => build_stream::<f64>(&device, &config, sender, Arc::clone(&state)),
        _ => Err(AudioError::StreamConfiguration),
    }?;
    stream.play().map_err(|_| AudioError::Stream)?;
    Ok(stream)
}

fn build_stream<T>(
    device: &Device,
    config: &StreamConfig,
    sender: mpsc::Sender<AudioFrame>,
    state: Arc<MicrophoneState>,
) -> Result<Stream, AudioError>
where
    T: InputSample + SizedSample,
{
    let channels = usize::from(config.channels);
    if channels == 0 {
        return Err(AudioError::StreamConfiguration);
    }
    let mut converter = InputConverter::new(channels, config.sample_rate.0);
    let error_state = Arc::clone(&state);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let samples = converter.convert(data);
                if samples.is_empty() {
                    return;
                }
                enqueue_frame(&sender, &state, AudioFrame::new(samples));
            },
            move |_| error_state.fail(FAILURE_STREAM),
            None,
        )
        .map_err(|_| AudioError::StreamConfiguration)
}

fn enqueue_frame(sender: &mpsc::Sender<AudioFrame>, state: &MicrophoneState, frame: AudioFrame) {
    if sender.try_send(frame).is_err() {
        state.fail(FAILURE_OVERFLOW);
    }
}

struct InputConverter {
    channels: usize,
    resampler: Pcm16Resampler,
}

impl InputConverter {
    fn new(channels: usize, input_sample_rate: u32) -> Self {
        Self {
            channels,
            resampler: Pcm16Resampler::new(input_sample_rate),
        }
    }

    fn convert<T>(&mut self, data: &[T]) -> Vec<i16>
    where
        T: InputSample,
    {
        let mono = data
            .chunks_exact(self.channels)
            .map(|frame| frame.iter().map(InputSample::to_f32).sum::<f32>() / self.channels as f32);
        self.resampler.process_mono(mono)
    }
}

trait InputSample: Copy + Send + 'static {
    fn to_f32(&self) -> f32;
}

macro_rules! signed_sample {
    ($type:ty) => {
        impl InputSample for $type {
            fn to_f32(&self) -> f32 {
                (*self as f64 / <$type>::MAX as f64).clamp(-1.0, 1.0) as f32
            }
        }
    };
}

macro_rules! unsigned_sample {
    ($type:ty) => {
        impl InputSample for $type {
            fn to_f32(&self) -> f32 {
                let midpoint = (<$type>::MAX as f64 + 1.0) / 2.0;
                ((*self as f64 - midpoint) / (midpoint - 1.0)).clamp(-1.0, 1.0) as f32
            }
        }
    };
}

signed_sample!(i8);
signed_sample!(i16);
signed_sample!(i32);
signed_sample!(i64);
unsigned_sample!(u8);
unsigned_sample!(u16);
unsigned_sample!(u32);
unsigned_sample!(u64);

impl InputSample for f32 {
    fn to_f32(&self) -> f32 {
        *self
    }
}

impl InputSample for f64 {
    fn to_f32(&self) -> f32 {
        self.clamp(-1.0, 1.0) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn microphone_with_capacity(
        capacity: usize,
    ) -> (
        MacOsMicrophone,
        mpsc::Sender<AudioFrame>,
        Arc<MicrophoneState>,
    ) {
        let state = Arc::new(MicrophoneState::default());
        let (sender, frames) = mpsc::channel(capacity);
        (
            MacOsMicrophone {
                frames,
                state: Arc::clone(&state),
                worker: None,
            },
            sender,
            state,
        )
    }

    #[tokio::test]
    async fn graceful_stop_drains_queued_frames_before_eof() {
        let (mut microphone, sender, state) = microphone_with_capacity(2);
        sender.try_send(AudioFrame::new(vec![1, 2])).unwrap();
        sender.try_send(AudioFrame::new(vec![3, 4])).unwrap();
        state.stop();
        drop(sender);
        let cancellation = CancellationToken::new();

        let first = microphone.next_frame(&cancellation).await.unwrap().unwrap();
        let second = microphone.next_frame(&cancellation).await.unwrap().unwrap();
        let end = microphone.next_frame(&cancellation).await.unwrap();

        assert_eq!(first.samples(), &[1, 2]);
        assert_eq!(second.samples(), &[3, 4]);
        assert!(end.is_none());
    }

    #[tokio::test]
    async fn overflow_remains_terminal_when_stop_is_also_signalled() {
        let (mut microphone, sender, state) = microphone_with_capacity(1);
        enqueue_frame(&sender, &state, AudioFrame::new(vec![1]));
        enqueue_frame(&sender, &state, AudioFrame::new(vec![2]));
        state.stop();
        let cancellation = CancellationToken::new();

        let result = microphone.next_frame(&cancellation).await;

        assert!(matches!(result, Err(AudioError::BufferOverflow)));
    }

    #[tokio::test]
    async fn stopped_queue_still_honors_cancellation_first() {
        let (mut microphone, sender, state) = microphone_with_capacity(1);
        sender.try_send(AudioFrame::new(vec![1])).unwrap();
        state.stop();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = microphone.next_frame(&cancellation).await;

        assert!(matches!(result, Err(AudioError::Cancelled)));
    }

    #[tokio::test]
    async fn startup_wait_is_cancellation_ready() {
        let (_sender, receiver) = tokio::sync::oneshot::channel();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = wait_for_startup(receiver, &cancellation, Duration::from_secs(1)).await;

        assert!(matches!(result, Err(StartupWaitError::Cancelled)));
    }

    #[tokio::test]
    async fn startup_wait_is_bounded() {
        let (_sender, receiver) = tokio::sync::oneshot::channel();
        let cancellation = CancellationToken::new();

        let result = wait_for_startup(receiver, &cancellation, Duration::from_millis(1)).await;

        assert!(matches!(result, Err(StartupWaitError::TimedOut)));
    }
}
