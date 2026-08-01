use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use reqwest::{Client, Method};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::{
    process::{CommandChild, CommandEvent},
    ShellExt,
};
use woof_core::{
    generate_health_challenge, verify_health_proof, ApiToken, WoofPaths, HEALTH_CHALLENGE_HEADER,
    HEALTH_PROOF_HEADER,
};

use crate::{commands, companion_panel, state::UiState};

const HEALTH_URL: &str = "http://127.0.0.1:3334/health";
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(2);
const HEALTH_CONNECT_TIMEOUT: Duration = Duration::from_millis(350);
const HEALTH_REQUEST_TIMEOUT: Duration = Duration::from_millis(900);
const UNHEALTHY_PROBE_LIMIT: u8 = 3;
const STABLE_HEALTH_PROBE_LIMIT: u8 = 3;
const RECOVERY_FAILURE_LIMIT: u32 = 3;
const RECOVERY_UNHEALTHY_PROBE_LIMIT: u8 = 2;
const CANONICAL_HEALTH_BODY: &[u8] = br#"{"status":"ok"}"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum HealthUiState {
    Healthy,
    Starting,
    Degraded,
    Offline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CaptureUiState {
    Active,
    Paused,
    Starting,
    PermissionRevoked,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct HealthChangedPayload {
    state: HealthUiState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct CaptureChangedPayload {
    state: CaptureUiState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct DatabaseRecoveryPayload {
    reason: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Stopped,
    Checking,
    Starting,
    OwnedHealthy,
    ExternalHealthy,
    Unhealthy,
    RestartWaiting,
}

impl Phase {
    fn status(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Checking | Self::Starting => "starting",
            Self::OwnedHealthy | Self::ExternalHealthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::RestartWaiting => "restarting",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct BackoffPolicy {
    initial: Duration,
    maximum: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(500),
            maximum: Duration::from_secs(30),
        }
    }
}

impl BackoffPolicy {
    fn delay(self, consecutive_failures: u32) -> Duration {
        let exponent = consecutive_failures.saturating_sub(1).min(31);
        self.initial
            .saturating_mul(1_u32 << exponent)
            .min(self.maximum)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeAction {
    None,
    Spawn,
    RestartOwned,
}

#[derive(Debug)]
struct SupervisorMachine {
    phase: Phase,
    owned_pid: Option<u32>,
    restart_count: u32,
    consecutive_failures: u32,
    healthy_probe_streak: u8,
    unhealthy_probe_streak: u8,
    last_exit_code: Option<i32>,
    last_exit_signal: Option<i32>,
    shutdown_requested: bool,
}

impl Default for SupervisorMachine {
    fn default() -> Self {
        Self {
            phase: Phase::Stopped,
            owned_pid: None,
            restart_count: 0,
            consecutive_failures: 0,
            healthy_probe_streak: 0,
            unhealthy_probe_streak: 0,
            last_exit_code: None,
            last_exit_signal: None,
            shutdown_requested: false,
        }
    }
}

impl SupervisorMachine {
    fn begin_check(&mut self) {
        if !self.shutdown_requested {
            self.phase = Phase::Checking;
        }
    }

    fn spawned(&mut self, pid: u32) {
        self.phase = Phase::Starting;
        self.owned_pid = Some(pid);
        self.healthy_probe_streak = 0;
        self.unhealthy_probe_streak = 0;
    }

    fn observe_health(&mut self, healthy: bool) -> ProbeAction {
        if self.shutdown_requested {
            return ProbeAction::None;
        }

        if healthy {
            self.unhealthy_probe_streak = 0;
            self.healthy_probe_streak = self.healthy_probe_streak.saturating_add(1);
            self.phase = if self.owned_pid.is_some() {
                Phase::OwnedHealthy
            } else {
                Phase::ExternalHealthy
            };
            if self.healthy_probe_streak >= STABLE_HEALTH_PROBE_LIMIT {
                self.consecutive_failures = 0;
            }
            return ProbeAction::None;
        }

        self.healthy_probe_streak = 0;
        self.unhealthy_probe_streak = self.unhealthy_probe_streak.saturating_add(1);
        if self.phase != Phase::RestartWaiting {
            self.phase = Phase::Unhealthy;
        }
        if self.unhealthy_probe_streak < UNHEALTHY_PROBE_LIMIT {
            return ProbeAction::None;
        }
        self.unhealthy_probe_streak = 0;

        if self.owned_pid.is_some() {
            ProbeAction::RestartOwned
        } else if self.phase == Phase::RestartWaiting {
            ProbeAction::None
        } else {
            ProbeAction::Spawn
        }
    }

    fn record_failure(
        &mut self,
        policy: BackoffPolicy,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
    ) -> Option<Duration> {
        self.owned_pid = None;
        self.healthy_probe_streak = 0;
        self.unhealthy_probe_streak = 0;
        self.last_exit_code = exit_code;
        self.last_exit_signal = exit_signal;
        if self.shutdown_requested {
            self.phase = Phase::Stopped;
            return None;
        }

        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.restart_count = self.restart_count.saturating_add(1);
        self.phase = Phase::RestartWaiting;
        Some(policy.delay(self.consecutive_failures))
    }

    fn prepare_manual_restart(&mut self) {
        self.shutdown_requested = false;
        self.phase = Phase::Checking;
        self.owned_pid = None;
        self.consecutive_failures = 0;
        self.healthy_probe_streak = 0;
        self.unhealthy_probe_streak = 0;
    }

    fn begin_shutdown(&mut self) {
        self.shutdown_requested = true;
        self.phase = Phase::Stopped;
        self.owned_pid = None;
        self.healthy_probe_streak = 0;
        self.unhealthy_probe_streak = 0;
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonSupervisorSnapshot {
    pub status: String,
    pub healthy: bool,
    pub ownership: String,
    pub pid: Option<u32>,
    pub restart_count: u32,
    pub consecutive_failures: u32,
    pub next_restart_ms: Option<u64>,
    pub last_exit_code: Option<i32>,
    pub last_exit_signal: Option<i32>,
}

#[derive(Default)]
struct SupervisorInner {
    machine: SupervisorMachine,
    child: Option<CommandChild>,
    generation: u64,
    spawning: bool,
    restart_deadline: Option<Instant>,
    capture_pause_sync: CapturePauseSync,
    observed_capture_state: Option<CaptureUiState>,
    last_emitted_health_state: Option<HealthUiState>,
    last_emitted_capture_state: Option<CaptureUiState>,
    health_recovery_visible: bool,
    permission_recovery_visible: bool,
    announced_database_recovery: Option<&'static str>,
}

#[derive(Default)]
struct CapturePauseSync {
    applied_generation: Option<u64>,
    in_flight_generation: Option<u64>,
}

impl CapturePauseSync {
    fn begin(&mut self, generation: u64, healthy: bool) -> bool {
        if !healthy
            || self.applied_generation == Some(generation)
            || self.in_flight_generation == Some(generation)
        {
            return false;
        }
        self.in_flight_generation = Some(generation);
        true
    }

    fn finish(&mut self, generation: u64, succeeded: bool) {
        if self.in_flight_generation != Some(generation) {
            return;
        }
        self.in_flight_generation = None;
        if succeeded {
            self.applied_generation = Some(generation);
        }
    }
}

#[derive(Clone)]
pub struct DaemonSupervisor {
    inner: Arc<Mutex<SupervisorInner>>,
    health_client: Client,
    health_token: ApiToken,
    backoff: BackoffPolicy,
}

impl DaemonSupervisor {
    pub fn new() -> Result<Self, String> {
        let paths =
            WoofPaths::discover().ok_or_else(|| "home directory is unavailable".to_string())?;
        let health_token = ApiToken::load_or_replace_invalid(&paths.token_path)
            .map_err(|_| "local daemon identity is unavailable".to_string())?;
        let health_client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(HEALTH_CONNECT_TIMEOUT)
            .timeout(HEALTH_REQUEST_TIMEOUT)
            .build()
            .map_err(|_| "could not initialize daemon supervision")?;
        Ok(Self {
            inner: Arc::new(Mutex::new(SupervisorInner::default())),
            health_client,
            health_token,
            backoff: BackoffPolicy::default(),
        })
    }

    pub fn start(&self, app: AppHandle) {
        let supervisor = self.clone();
        tauri::async_runtime::spawn(async move {
            supervisor.ensure_running(app.clone(), true).await;
            supervisor.monitor(app).await;
        });
    }

    pub fn snapshot(&self) -> DaemonSupervisorSnapshot {
        let Ok(inner) = self.inner.lock() else {
            return DaemonSupervisorSnapshot {
                status: "unavailable".into(),
                healthy: false,
                ownership: "none".into(),
                pid: None,
                restart_count: 0,
                consecutive_failures: 0,
                next_restart_ms: None,
                last_exit_code: None,
                last_exit_signal: None,
            };
        };
        let next_restart_ms = inner.restart_deadline.map(|deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(u128::from(u64::MAX)) as u64
        });
        let ownership = if inner.machine.owned_pid.is_some() {
            "owned"
        } else if inner.machine.phase == Phase::ExternalHealthy {
            "external"
        } else {
            "none"
        };
        DaemonSupervisorSnapshot {
            status: inner.machine.phase.status().into(),
            healthy: matches!(
                inner.machine.phase,
                Phase::OwnedHealthy | Phase::ExternalHealthy
            ),
            ownership: ownership.into(),
            pid: inner.machine.owned_pid,
            restart_count: inner.machine.restart_count,
            consecutive_failures: inner.machine.consecutive_failures,
            next_restart_ms,
            last_exit_code: inner.machine.last_exit_code,
            last_exit_signal: inner.machine.last_exit_signal,
        }
    }

    pub async fn refresh(&self, app: AppHandle) -> DaemonSupervisorSnapshot {
        self.probe_and_act(app).await;
        self.snapshot()
    }

    pub async fn restart(&self, app: AppHandle) -> DaemonSupervisorSnapshot {
        let child = {
            let Ok(mut inner) = self.inner.lock() else {
                return self.snapshot();
            };
            inner.generation = inner.generation.wrapping_add(1);
            inner.spawning = false;
            inner.restart_deadline = None;
            inner.machine.prepare_manual_restart();
            inner.child.take()
        };
        self.publish_runtime_state(&app, None);
        if let Some(child) = child {
            let _ = child.kill();
            for _ in 0..5 {
                if !self.probe_health().await {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        self.ensure_running(app, true).await;
        self.snapshot()
    }

    pub fn shutdown(&self) {
        let child = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            inner.generation = inner.generation.wrapping_add(1);
            inner.spawning = false;
            inner.restart_deadline = None;
            inner.machine.begin_shutdown();
            inner.child.take()
        };
        if let Some(child) = child {
            let _ = child.kill();
        }
    }

    async fn monitor(&self, app: AppHandle) {
        loop {
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
            if self.shutdown_requested() {
                return;
            }
            self.probe_and_act(app.clone()).await;
        }
    }

    async fn probe_and_act(&self, app: AppHandle) {
        let healthy = self.probe_health().await;
        let action = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            let action = inner.machine.observe_health(healthy);
            if healthy && inner.child.is_none() && inner.restart_deadline.take().is_some() {
                inner.generation = inner.generation.wrapping_add(1);
            }
            action
        };
        match action {
            ProbeAction::None => {}
            ProbeAction::Spawn => self.ensure_running(app.clone(), false).await,
            ProbeAction::RestartOwned => self.restart_unhealthy(app.clone()),
        }
        let (observed_capture_state, database_recovery) = if healthy {
            self.synchronize_capture_pause(&app).await;
            let (capture_state, database_recovery) = self.read_capture_state().await;
            (Some(capture_state), database_recovery)
        } else {
            (None, None)
        };
        self.publish_runtime_state(&app, observed_capture_state);
        if let Some(reason) = database_recovery {
            self.publish_database_recovery(&app, reason);
        }
    }

    async fn ensure_running(&self, app: AppHandle, bypass_restart_wait: bool) {
        let attempt_generation = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            if inner.machine.shutdown_requested
                || inner.child.is_some()
                || inner.spawning
                || (!bypass_restart_wait && inner.machine.phase == Phase::RestartWaiting)
            {
                return;
            }
            inner.generation = inner.generation.wrapping_add(1);
            inner.spawning = true;
            inner.restart_deadline = None;
            inner.machine.begin_check();
            inner.generation
        };
        self.publish_runtime_state(&app, None);

        if self.probe_health().await {
            let mut accepted = false;
            if let Ok(mut inner) = self.inner.lock() {
                if inner.generation == attempt_generation && inner.spawning {
                    inner.spawning = false;
                    inner.machine.observe_health(true);
                    accepted = true;
                }
            }
            if accepted {
                self.synchronize_capture_pause(&app).await;
            }
            let (observed_capture_state, database_recovery) = if accepted {
                let (capture_state, database_recovery) = self.read_capture_state().await;
                (Some(capture_state), database_recovery)
            } else {
                (None, None)
            };
            self.publish_runtime_state(&app, observed_capture_state);
            if let Some(reason) = database_recovery {
                self.publish_database_recovery(&app, reason);
            }
            return;
        }

        let start_paused = app
            .state::<UiState>()
            .read()
            .map_or(true, |preferences| preferences.capture_paused);
        let spawn_result = app
            .shell()
            .sidecar("woof_d")
            .map(|command| command.args(daemon_sidecar_args(start_paused)))
            .and_then(tauri_plugin_shell::process::Command::spawn);
        let mut stale_child = None;
        let mut event_receiver = None;
        let mut spawned_generation = None;
        let mut restart = None;

        {
            let Ok(mut inner) = self.inner.lock() else {
                if let Ok((_receiver, child)) = spawn_result {
                    let _ = child.kill();
                }
                return;
            };
            if inner.generation != attempt_generation
                || !inner.spawning
                || inner.machine.shutdown_requested
            {
                if let Ok((_receiver, child)) = spawn_result {
                    stale_child = Some(child);
                }
            } else {
                inner.spawning = false;
                match spawn_result {
                    Ok((receiver, child)) => {
                        let pid = child.pid();
                        inner.child = Some(child);
                        inner.machine.spawned(pid);
                        event_receiver = Some(receiver);
                        spawned_generation = Some(inner.generation);
                    }
                    Err(_) => {
                        inner.generation = inner.generation.wrapping_add(1);
                        let generation = inner.generation;
                        if let Some(delay) = inner.machine.record_failure(self.backoff, None, None)
                        {
                            inner.restart_deadline = Some(Instant::now() + delay);
                            restart = Some((generation, delay));
                        }
                    }
                }
            }
        }

        if let Some(child) = stale_child {
            let _ = child.kill();
        }
        if let (Some(mut receiver), Some(generation)) = (event_receiver, spawned_generation) {
            let supervisor = self.clone();
            let handle = app.clone();
            tauri::async_runtime::spawn(async move {
                while let Some(event) = receiver.recv().await {
                    match event {
                        CommandEvent::Stdout(_)
                        | CommandEvent::Stderr(_)
                        | CommandEvent::Error(_) => {
                            // Sidecar output may contain captured content. Drain it without logging.
                        }
                        CommandEvent::Terminated(payload) => {
                            supervisor.handle_terminated(
                                handle,
                                generation,
                                payload.code,
                                payload.signal,
                            );
                            return;
                        }
                        _ => {}
                    }
                }
                supervisor.handle_terminated(handle, generation, None, None);
            });
        }
        if let Some((generation, delay)) = restart {
            self.schedule_restart(app, generation, delay);
        } else {
            self.publish_runtime_state(&app, None);
        }
    }

    fn handle_terminated(
        &self,
        app: AppHandle,
        generation: u64,
        exit_code: Option<i32>,
        exit_signal: Option<i32>,
    ) {
        let restart = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            if inner.generation != generation {
                return;
            }
            inner.child.take();
            inner.generation = inner.generation.wrapping_add(1);
            let restart_generation = inner.generation;
            inner
                .machine
                .record_failure(self.backoff, exit_code, exit_signal)
                .map(|delay| {
                    inner.restart_deadline = Some(Instant::now() + delay);
                    (restart_generation, delay)
                })
        };
        if let Some((restart_generation, delay)) = restart {
            self.schedule_restart(app, restart_generation, delay);
        } else {
            self.publish_runtime_state(&app, None);
        }
    }

    fn restart_unhealthy(&self, app: AppHandle) {
        let (child, restart) = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            let Some(child) = inner.child.take() else {
                return;
            };
            inner.generation = inner.generation.wrapping_add(1);
            let restart_generation = inner.generation;
            let restart = inner
                .machine
                .record_failure(self.backoff, None, None)
                .map(|delay| {
                    inner.restart_deadline = Some(Instant::now() + delay);
                    (restart_generation, delay)
                });
            (child, restart)
        };
        let _ = child.kill();
        if let Some((generation, delay)) = restart {
            self.schedule_restart(app, generation, delay);
        }
    }

    fn schedule_restart(&self, app: AppHandle, generation: u64, delay: Duration) {
        self.publish_runtime_state(&app, None);
        let supervisor = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(delay).await;
            {
                let Ok(mut inner) = supervisor.inner.lock() else {
                    return;
                };
                if inner.generation != generation
                    || inner.machine.shutdown_requested
                    || inner.machine.phase != Phase::RestartWaiting
                {
                    return;
                }
                inner.restart_deadline = None;
                inner.machine.begin_check();
            }
            supervisor.ensure_running(app, true).await;
        });
    }

    fn publish_runtime_state(
        &self,
        app: &AppHandle,
        observed_capture_state: Option<CaptureUiState>,
    ) {
        let permission_recovery_allowed = app
            .state::<UiState>()
            .read()
            .is_ok_and(|preferences| preferences.onboarding_done && !preferences.capture_paused);
        let publication = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            if let Some(capture_state) = observed_capture_state {
                inner.observed_capture_state = Some(capture_state);
            }

            let capture_state =
                effective_capture_state(inner.machine.phase, inner.observed_capture_state);
            let previous_visibility = RecoveryVisibility {
                health: inner.health_recovery_visible,
                permission: inner.permission_recovery_visible,
            };
            let visibility = next_recovery_visibility(
                previous_visibility,
                &inner.machine,
                capture_state,
                permission_recovery_allowed,
            );
            let health_state = health_ui_state(&inner.machine, capture_state, visibility.health);
            let emit_capture = (inner.last_emitted_capture_state != Some(capture_state)).then_some(
                CaptureChangedPayload {
                    state: capture_state,
                },
            );
            let emit_health = (inner.last_emitted_health_state != Some(health_state)).then_some(
                HealthChangedPayload {
                    state: health_state,
                },
            );

            inner.last_emitted_capture_state = Some(capture_state);
            inner.last_emitted_health_state = Some(health_state);
            inner.health_recovery_visible = visibility.health;
            inner.permission_recovery_visible = visibility.permission;

            RuntimePublication {
                emit_capture,
                emit_health,
                show_health: !previous_visibility.health && visibility.health,
                hide_health: previous_visibility.health && !visibility.health,
                show_permission: !previous_visibility.permission && visibility.permission,
                hide_permission: previous_visibility.permission && !visibility.permission,
            }
        };

        if let Some(payload) = publication.emit_capture {
            for label in [companion_panel::WINDOW_LABEL, "memory-hub"] {
                if let Some(window) = app.get_webview_window(label) {
                    let _ = window.emit("woof:capture-changed", payload);
                }
            }
        }
        if let Some(payload) = publication.emit_health {
            for label in [companion_panel::WINDOW_LABEL, "memory-hub", "health"] {
                if let Some(window) = app.get_webview_window(label) {
                    let _ = window.emit("woof:health-changed", payload);
                }
            }
        }

        if publication.hide_permission {
            hide_window(app, "permission");
        }
        if publication.hide_health {
            hide_window(app, "health");
        }
        if publication.show_health {
            show_focused_window(app, "health");
        }
        if publication.show_permission {
            show_focused_window(app, "permission");
        }
    }

    fn publish_database_recovery(&self, app: &AppHandle, reason: &'static str) {
        let should_publish = self.inner.lock().is_ok_and(|mut inner| {
            if inner.announced_database_recovery.is_some() {
                return false;
            }
            inner.announced_database_recovery = Some(reason);
            true
        });
        if !should_publish {
            return;
        }

        let payload = DatabaseRecoveryPayload { reason };
        for label in [companion_panel::WINDOW_LABEL, "memory-hub", "health"] {
            if let Some(window) = app.get_webview_window(label) {
                let _ = window.emit("woof:database-reset", payload);
            }
        }
        show_focused_window(app, "health");
    }

    async fn read_capture_state(&self) -> (CaptureUiState, Option<&'static str>) {
        commands::daemon_request(Method::GET, "/capture/status", None)
            .await
            .map_or((CaptureUiState::Error, None), |status| {
                (capture_ui_state(&status), database_recovery_reason(&status))
            })
    }

    async fn probe_health(&self) -> bool {
        let challenge = generate_health_challenge();
        let Ok(mut response) = self
            .health_client
            .get(HEALTH_URL)
            .header(HEALTH_CHALLENGE_HEADER, &challenge)
            .send()
            .await
        else {
            return false;
        };
        if response.status() != reqwest::StatusCode::OK {
            return false;
        }
        let Some(proof) = response
            .headers()
            .get(HEALTH_PROOF_HEADER)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        if !verify_health_proof(&self.health_token, &challenge, proof) {
            return false;
        }
        if response
            .content_length()
            .is_some_and(|length| length != CANONICAL_HEALTH_BODY.len() as u64)
        {
            return false;
        }
        let mut offset = 0;
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    let Some(next) = match_health_body_chunk(offset, &chunk) else {
                        return false;
                    };
                    offset = next;
                }
                Ok(None) => return offset == CANONICAL_HEALTH_BODY.len(),
                Err(_) => return false,
            }
        }
    }

    async fn synchronize_capture_pause(&self, app: &AppHandle) {
        let generation = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            let generation = inner.generation;
            let healthy = matches!(
                inner.machine.phase,
                Phase::OwnedHealthy | Phase::ExternalHealthy
            );
            if !inner.capture_pause_sync.begin(generation, healthy) {
                return;
            }
            generation
        };
        let succeeded = commands::synchronize_persisted_capture_pause(app)
            .await
            .is_ok();
        if let Ok(mut inner) = self.inner.lock() {
            if inner.generation == generation {
                inner.capture_pause_sync.finish(generation, succeeded);
            }
        }
    }

    fn shutdown_requested(&self) -> bool {
        self.inner
            .lock()
            .map_or(true, |inner| inner.machine.shutdown_requested)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RecoveryVisibility {
    health: bool,
    permission: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct RuntimePublication {
    emit_capture: Option<CaptureChangedPayload>,
    emit_health: Option<HealthChangedPayload>,
    show_health: bool,
    hide_health: bool,
    show_permission: bool,
    hide_permission: bool,
}

pub(crate) fn capture_ui_state(status: &Value) -> CaptureUiState {
    let Some(runtime) = status.get("runtime") else {
        return CaptureUiState::Error;
    };
    let permission = runtime.get("permission").and_then(Value::as_str);
    let last_error = runtime.get("last_error").and_then(Value::as_str);

    if status
        .pointer("/accessibility/trusted")
        .and_then(Value::as_bool)
        == Some(false)
        || permission == Some("denied")
        || last_error == Some("permission_denied")
    {
        return CaptureUiState::PermissionRevoked;
    }
    if runtime.get("running").and_then(Value::as_bool) != Some(true) {
        return if permission == Some("unknown") {
            CaptureUiState::Starting
        } else {
            CaptureUiState::Error
        };
    }
    if status.get("paused").and_then(Value::as_bool) == Some(true) {
        return CaptureUiState::Paused;
    }
    if permission == Some("unknown") {
        return CaptureUiState::Starting;
    }
    if permission != Some("granted")
        || status.get("capturing").and_then(Value::as_bool) != Some(true)
    {
        return CaptureUiState::Error;
    }

    match last_error {
        None | Some("secure_input" | "no_focused_application" | "semantic_index") => {
            CaptureUiState::Active
        }
        Some(_) => CaptureUiState::Error,
    }
}

fn database_recovery_reason(status: &Value) -> Option<&'static str> {
    let recovery = status.get("database_recovery")?;
    if recovery.get("occurred").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    match recovery.get("reason").and_then(Value::as_str) {
        Some("corrupt") => Some("corrupt"),
        Some("incompatible-schema") => Some("incompatible-schema"),
        Some("unsupported-version") => Some("unsupported-version"),
        _ => None,
    }
}

fn effective_capture_state(
    phase: Phase,
    observed_capture_state: Option<CaptureUiState>,
) -> CaptureUiState {
    match phase {
        Phase::OwnedHealthy | Phase::ExternalHealthy => {
            observed_capture_state.unwrap_or(CaptureUiState::Starting)
        }
        Phase::Checking | Phase::Starting => CaptureUiState::Starting,
        Phase::Stopped | Phase::Unhealthy | Phase::RestartWaiting => CaptureUiState::Error,
    }
}

fn next_recovery_visibility(
    current: RecoveryVisibility,
    machine: &SupervisorMachine,
    capture_state: CaptureUiState,
    permission_recovery_allowed: bool,
) -> RecoveryVisibility {
    let daemon_healthy = matches!(machine.phase, Phase::OwnedHealthy | Phase::ExternalHealthy);
    let stably_restored = daemon_healthy
        && machine.healthy_probe_streak >= STABLE_HEALTH_PROBE_LIMIT
        && machine.consecutive_failures == 0;
    let sustained_failure = !daemon_healthy
        && (machine.consecutive_failures >= RECOVERY_FAILURE_LIMIT
            || (machine.phase == Phase::Unhealthy
                && machine.unhealthy_probe_streak >= RECOVERY_UNHEALTHY_PROBE_LIMIT));

    let health = if stably_restored {
        false
    } else if sustained_failure {
        true
    } else {
        current.health
    };
    let permission = !health
        && permission_recovery_allowed
        && if daemon_healthy {
            capture_state == CaptureUiState::PermissionRevoked
        } else {
            current.permission
        };
    RecoveryVisibility { health, permission }
}

fn health_ui_state(
    machine: &SupervisorMachine,
    capture_state: CaptureUiState,
    health_recovery_visible: bool,
) -> HealthUiState {
    match machine.phase {
        Phase::OwnedHealthy | Phase::ExternalHealthy => {
            if health_recovery_visible
                && (machine.healthy_probe_streak < STABLE_HEALTH_PROBE_LIMIT
                    || machine.consecutive_failures > 0)
            {
                return HealthUiState::Starting;
            }
            match capture_state {
                CaptureUiState::Active | CaptureUiState::Paused => HealthUiState::Healthy,
                CaptureUiState::Starting => HealthUiState::Starting,
                CaptureUiState::PermissionRevoked | CaptureUiState::Error => {
                    HealthUiState::Degraded
                }
            }
        }
        Phase::Checking | Phase::Starting => HealthUiState::Starting,
        Phase::Unhealthy => HealthUiState::Degraded,
        Phase::RestartWaiting if health_recovery_visible => HealthUiState::Offline,
        Phase::RestartWaiting => HealthUiState::Starting,
        Phase::Stopped => HealthUiState::Offline,
    }
}

fn hide_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.hide();
    }
}

fn show_focused_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn match_health_body_chunk(offset: usize, chunk: &[u8]) -> Option<usize> {
    let next = offset.checked_add(chunk.len())?;
    (next <= CANONICAL_HEALTH_BODY.len() && chunk == &CANONICAL_HEALTH_BODY[offset..next])
        .then_some(next)
}

fn daemon_sidecar_args(start_paused: bool) -> Vec<&'static str> {
    let mut arguments = vec!["--watch-parent-stdin"];
    if start_paused {
        arguments.push("--start-paused");
    }
    arguments
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exponential_backoff_is_capped() {
        let policy = BackoffPolicy::default();
        let delays: Vec<_> = (1..=8).map(|failure| policy.delay(failure)).collect();
        assert_eq!(
            delays,
            vec![
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
    }

    #[test]
    fn sidecar_starts_paused_before_capture_when_the_preference_is_paused() {
        assert_eq!(
            daemon_sidecar_args(true),
            vec!["--watch-parent-stdin", "--start-paused"]
        );
        assert_eq!(daemon_sidecar_args(false), vec!["--watch-parent-stdin"]);
    }

    #[test]
    fn health_proofs_are_bound_to_a_fresh_challenge_and_local_token() {
        let token = ApiToken::generate();
        let impostor = ApiToken::generate();
        let challenge = generate_health_challenge();
        let proof = woof_core::health_proof(&token, &challenge).unwrap();
        assert!(verify_health_proof(&token, &challenge, &proof));
        assert!(!verify_health_proof(&impostor, &challenge, &proof));
        assert!(!verify_health_proof(
            &token,
            &generate_health_challenge(),
            &proof
        ));
    }

    #[test]
    fn health_body_stream_accepts_only_the_exact_canonical_bytes() {
        let first = match_health_body_chunk(0, b"{\"status\":\"").unwrap();
        let end = match_health_body_chunk(first, b"ok\"}").unwrap();
        assert_eq!(end, CANONICAL_HEALTH_BODY.len());
        assert!(match_health_body_chunk(0, b" {\"status\":\"ok\"}").is_none());
        assert!(match_health_body_chunk(0, b"{\"status\":\"ok\"}\n").is_none());
        assert!(match_health_body_chunk(0, b"{\"status\":\"healthy\"}").is_none());
    }

    #[test]
    fn unexpected_exit_records_state_and_schedules_restart() {
        let mut machine = SupervisorMachine::default();
        machine.spawned(42);
        let delay = machine.record_failure(BackoffPolicy::default(), Some(7), None);
        assert_eq!(delay, Some(Duration::from_millis(500)));
        assert_eq!(machine.phase, Phase::RestartWaiting);
        assert_eq!(machine.owned_pid, None);
        assert_eq!(machine.restart_count, 1);
        assert_eq!(machine.consecutive_failures, 1);
        assert_eq!(machine.last_exit_code, Some(7));
    }

    #[test]
    fn shutdown_suppresses_restart_after_exit() {
        let mut machine = SupervisorMachine::default();
        machine.spawned(42);
        machine.begin_shutdown();
        let delay = machine.record_failure(BackoffPolicy::default(), None, Some(15));
        assert_eq!(delay, None);
        assert_eq!(machine.phase, Phase::Stopped);
        assert_eq!(machine.restart_count, 0);
    }

    #[test]
    fn stable_health_resets_backoff_failures() {
        let mut machine = SupervisorMachine::default();
        machine.spawned(42);
        let _ = machine.record_failure(BackoffPolicy::default(), Some(1), None);
        machine.spawned(43);
        for _ in 0..STABLE_HEALTH_PROBE_LIMIT {
            assert_eq!(machine.observe_health(true), ProbeAction::None);
        }
        assert_eq!(machine.phase, Phase::OwnedHealthy);
        assert_eq!(machine.consecutive_failures, 0);
        assert_eq!(machine.restart_count, 1);
    }

    #[test]
    fn external_daemon_is_not_claimed_as_owned() {
        let mut machine = SupervisorMachine::default();
        machine.begin_check();
        assert_eq!(machine.observe_health(true), ProbeAction::None);
        assert_eq!(machine.phase, Phase::ExternalHealthy);
        assert_eq!(machine.owned_pid, None);
    }

    #[test]
    fn repeated_unhealthy_probes_restart_only_owned_processes() {
        let mut owned = SupervisorMachine::default();
        owned.spawned(42);
        for _ in 1..UNHEALTHY_PROBE_LIMIT {
            assert_eq!(owned.observe_health(false), ProbeAction::None);
        }
        assert_eq!(owned.observe_health(false), ProbeAction::RestartOwned);

        let mut external = SupervisorMachine::default();
        external.observe_health(true);
        for _ in 1..UNHEALTHY_PROBE_LIMIT {
            assert_eq!(external.observe_health(false), ProbeAction::None);
        }
        assert_eq!(external.observe_health(false), ProbeAction::Spawn);
        assert_eq!(external.owned_pid, None);
    }

    #[test]
    fn persisted_pause_is_applied_once_per_healthy_daemon_generation() {
        let mut sync = CapturePauseSync::default();
        assert!(!sync.begin(1, false));
        assert!(sync.begin(1, true));
        assert!(!sync.begin(1, true));
        sync.finish(1, true);
        assert!(!sync.begin(1, true));
        assert!(sync.begin(2, true));
    }

    #[test]
    fn failed_pause_reapplication_is_retried() {
        let mut sync = CapturePauseSync::default();
        assert!(sync.begin(7, true));
        sync.finish(7, false);
        assert!(sync.begin(7, true));
    }

    #[test]
    fn capture_status_maps_permission_and_runtime_without_false_active_states() {
        assert_eq!(
            capture_ui_state(&json!({
                "paused": false,
                "capturing": false,
                "accessibility": {"trusted": false, "operational": false, "ready": false},
                "runtime": {"running": true, "permission": "granted", "last_error": null}
            })),
            CaptureUiState::PermissionRevoked
        );
        assert_eq!(
            capture_ui_state(&json!({
                "paused": false,
                "capturing": true,
                "runtime": {"running": true, "permission": "denied", "last_error": null}
            })),
            CaptureUiState::PermissionRevoked
        );
        assert_eq!(
            capture_ui_state(&json!({
                "paused": true,
                "capturing": false,
                "runtime": {"running": true, "permission": "denied", "last_error": "permission_denied"}
            })),
            CaptureUiState::PermissionRevoked
        );
        assert_eq!(
            capture_ui_state(&json!({
                "paused": true,
                "capturing": false,
                "runtime": {"running": true, "permission": "granted", "last_error": null}
            })),
            CaptureUiState::Paused
        );
        assert_eq!(
            capture_ui_state(&json!({
                "paused": false,
                "capturing": true,
                "runtime": {"running": true, "permission": "unknown", "last_error": null}
            })),
            CaptureUiState::Starting
        );
        assert_eq!(
            capture_ui_state(&json!({
                "paused": false,
                "capturing": false,
                "runtime": {"running": false, "permission": "unknown", "last_error": null}
            })),
            CaptureUiState::Starting
        );
        assert_eq!(
            capture_ui_state(&json!({
                "paused": false,
                "capturing": false,
                "runtime": {"running": false, "permission": "granted", "last_error": null}
            })),
            CaptureUiState::Error
        );
        assert_eq!(
            capture_ui_state(&json!({
                "paused": false,
                "capturing": true,
                "runtime": {"running": true, "permission": "granted", "last_error": null}
            })),
            CaptureUiState::Active
        );
        for expected_transient in ["secure_input", "no_focused_application", "semantic_index"] {
            assert_eq!(
                capture_ui_state(&json!({
                    "paused": false,
                    "capturing": true,
                    "runtime": {
                        "running": true,
                        "permission": "granted",
                        "last_error": expected_transient
                    }
                })),
                CaptureUiState::Active
            );
        }
        for expected_error in ["storage", "accessibility", "unexpected"] {
            assert_eq!(
                capture_ui_state(&json!({
                    "paused": false,
                    "capturing": true,
                    "runtime": {
                        "running": true,
                        "permission": "granted",
                        "last_error": expected_error
                    }
                })),
                CaptureUiState::Error
            );
        }
    }

    #[test]
    fn database_recovery_signal_accepts_only_the_bounded_daemon_contract() {
        for reason in ["corrupt", "incompatible-schema", "unsupported-version"] {
            assert_eq!(
                database_recovery_reason(&json!({
                    "database_recovery": {"occurred": true, "reason": reason}
                })),
                Some(reason)
            );
        }
        for recovery in [
            json!(null),
            json!({"occurred": false, "reason": "corrupt"}),
            json!({"occurred": true, "reason": "private/path"}),
            json!({"occurred": true, "reason": 42}),
        ] {
            assert_eq!(
                database_recovery_reason(&json!({"database_recovery": recovery})),
                None
            );
        }
    }

    #[test]
    fn sustained_daemon_failures_latch_recovery_until_stable_health() {
        let mut machine = SupervisorMachine::default();
        machine.begin_check();
        assert_eq!(machine.observe_health(false), ProbeAction::None);
        let transient = next_recovery_visibility(
            RecoveryVisibility::default(),
            &machine,
            CaptureUiState::Error,
            true,
        );
        assert!(!transient.health);

        assert_eq!(machine.observe_health(false), ProbeAction::None);
        let latched = next_recovery_visibility(transient, &machine, CaptureUiState::Error, true);
        assert!(latched.health);
        assert!(!latched.permission);

        assert_eq!(machine.observe_health(true), ProbeAction::None);
        let recovering = next_recovery_visibility(latched, &machine, CaptureUiState::Active, true);
        assert!(recovering.health);
        assert_eq!(
            health_ui_state(&machine, CaptureUiState::Active, recovering.health),
            HealthUiState::Starting
        );

        for _ in 1..STABLE_HEALTH_PROBE_LIMIT {
            assert_eq!(machine.observe_health(true), ProbeAction::None);
        }
        let restored = next_recovery_visibility(recovering, &machine, CaptureUiState::Active, true);
        assert_eq!(restored, RecoveryVisibility::default());
    }

    #[test]
    fn restart_loops_reach_offline_recovery_without_classifying_private_errors() {
        let mut machine = SupervisorMachine::default();
        for pid in 1..=RECOVERY_FAILURE_LIMIT {
            machine.spawned(pid);
            assert!(machine
                .record_failure(BackoffPolicy::default(), Some(1), None)
                .is_some());
        }
        let visibility = next_recovery_visibility(
            RecoveryVisibility::default(),
            &machine,
            CaptureUiState::Error,
            true,
        );
        assert!(visibility.health);
        assert_eq!(
            health_ui_state(&machine, CaptureUiState::Error, visibility.health),
            HealthUiState::Offline
        );
    }

    #[test]
    fn permission_recovery_is_gated_debounced_and_mutually_exclusive() {
        let mut machine = SupervisorMachine::default();
        machine.begin_check();
        assert_eq!(machine.observe_health(true), ProbeAction::None);

        let gated = next_recovery_visibility(
            RecoveryVisibility::default(),
            &machine,
            CaptureUiState::PermissionRevoked,
            false,
        );
        assert_eq!(gated, RecoveryVisibility::default());

        let visible =
            next_recovery_visibility(gated, &machine, CaptureUiState::PermissionRevoked, true);
        assert_eq!(
            visible,
            RecoveryVisibility {
                health: false,
                permission: true
            }
        );
        assert_eq!(machine.observe_health(false), ProbeAction::None);
        let held_through_transient_probe =
            next_recovery_visibility(visible, &machine, CaptureUiState::Error, true);
        assert!(held_through_transient_probe.permission);
        assert_eq!(machine.observe_health(true), ProbeAction::None);
        assert_eq!(
            next_recovery_visibility(
                held_through_transient_probe,
                &machine,
                CaptureUiState::Active,
                true
            ),
            RecoveryVisibility::default()
        );

        let health_wins = next_recovery_visibility(
            RecoveryVisibility {
                health: true,
                permission: true,
            },
            &machine,
            CaptureUiState::PermissionRevoked,
            true,
        );
        assert!(health_wins.health);
        assert!(!health_wins.permission);
    }

    #[test]
    fn native_states_serialize_to_the_frontend_contract() {
        assert_eq!(
            serde_json::to_value(HealthChangedPayload {
                state: HealthUiState::Healthy
            })
            .unwrap(),
            json!({"state": "healthy"})
        );
        assert_eq!(
            serde_json::to_value(CaptureChangedPayload {
                state: CaptureUiState::PermissionRevoked
            })
            .unwrap(),
            json!({"state": "permission-revoked"})
        );
    }
}
