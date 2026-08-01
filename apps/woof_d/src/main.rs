use std::{future::Future, net::SocketAddr, process::ExitCode, sync::Arc};

#[cfg(unix)]
use std::os::fd::{AsFd, OwnedFd};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing_subscriber::EnvFilter;
use woof_core::{ApiToken, WoofConfig, WoofPaths};
use woof_storage::Storage;

#[cfg(target_os = "macos")]
const PRIVATE_FILE_UMASK: u16 = 0o077;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn umask(mask: u16) -> u16;
}

fn restrict_private_file_creation() {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: umask changes only this process's file-creation mask. Calling
        // it before any woof path is opened ensures SQLite sidecars and all
        // other new runtime files start without group or world permissions.
        unsafe {
            umask(PRIVATE_FILE_UMASK);
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    restrict_private_file_creation();
    if let Err(error) = run().await {
        eprintln!("woof_d failed: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = daemon_options(std::env::args().skip(1))?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .without_time()
        .init();

    let paths = WoofPaths::discover().ok_or("could not discover the macOS home directory")?;
    let config = WoofConfig::load_or_replace_invalid(&paths)?;
    let address: SocketAddr = config.bind_address().parse()?;
    // Listener ownership is the startup lock. Acquire it before opening or
    // reconciling SQLite so a second daemon cannot mutate durable state.
    #[allow(clippy::redundant_closure)] // Keep the exact bind visible to the boundary audit.
    let (listener, storage_startup) = bind_before_storage(
        address,
        |address| tokio::net::TcpListener::bind(address),
        || Storage::open_or_recover(&config.db_path).map_err(Into::into),
    )
    .await?;
    let listener_guard = retain_listener_lock(&listener)?;
    let token = ApiToken::load_or_replace_invalid(&paths.token_path)?;
    if let Some(recovery) = &storage_startup.recovery {
        tracing::warn!(
            reason = recovery.reason.diagnostic_code(),
            quarantine = "local data directory/database-quarantine",
            "isolated an unusable database and initialized fresh local storage"
        );
    }
    let database_recovery = storage_startup
        .recovery
        .as_ref()
        .map(|recovery| recovery.reason);
    let storage = storage_startup.storage;
    if let Some(cutoff) = config.data_retention.cutoff(chrono::Utc::now().timestamp()) {
        let retention_storage = storage.clone();
        let report =
            tokio::task::spawn_blocking(move || retention_storage.prune_expired_data(cutoff))
                .await??;
        if report.deleted_rows > 0 {
            tracing::info!(
                deleted_rows = report.deleted_rows,
                expired_snapshots = report.expired_snapshots,
                "enforced data retention at startup"
            );
        }
    }
    let semantic_storage = storage.clone();
    let vector_index_path = paths.vector_index_path.clone();
    let (semantic, initialization) = tokio::task::spawn_blocking(move || {
        woof_d::SemanticSearchService::initialize_local(&semantic_storage, vector_index_path)
    })
    .await??;
    match initialization {
        woof_d::SemanticInitialization::Loaded { indexed } => {
            tracing::info!(indexed, "loaded local semantic index");
        }
        woof_d::SemanticInitialization::Rebuilt(report) => {
            tracing::info!(
                indexed = report.indexed,
                skipped_empty = report.skipped_empty,
                "rebuilt local semantic index"
            );
        }
    }
    let memory_storage = storage.clone();
    let state = woof_d::AppState::new(storage, token)
        .with_initial_blacklist(config.capture_blacklist.clone())
        .with_semantic_search(semantic)
        .with_database_recovery(database_recovery)
        .with_persisted_config(paths.config_path.clone(), config.clone());
    if options.start_paused {
        state.pause_capture();
    }

    let supervisor = woof_d::spawn_capture_service(
        state.clone(),
        std::time::Duration::from_millis(config.capture_interval_ms.max(100)),
        std::time::Duration::from_secs(config.coalesce_window_secs.max(1)),
        config.working_memory_capacity,
    )
    .await;
    let automation_supervisor =
        woof_d::spawn_automation_service(state.clone(), std::time::Duration::from_secs(30));
    let memory_mutation_barrier = state.storage_mutation_barrier();
    let memory_generation_gate = state.memory_generation_gate();

    // Start polling the already-bound local listener before the memory
    // scheduler can perform its first lazy Keychain read. That read may wait
    // for a native SecurityAgent decision, but local health, capture,
    // automation, and authenticated APIs must remain available meanwhile.
    let server_state = state.clone();
    tracing::info!(address = %address, "woof daemon listening");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, woof_d::router(server_state))
            .with_graceful_shutdown(shutdown_signal(options.watch_parent_stdin))
            .await
    });
    if let Err(error) = await_local_health(address).await {
        server_task.abort();
        supervisor.shutdown().await;
        automation_supervisor.shutdown().await;
        let _ = server_task.await;
        let shutdown_mutation_guard = state.storage_mutation_barrier().lock().await;
        drop(listener_guard);
        drop(shutdown_mutation_guard);
        return Err(error);
    }

    let memory_supervisor = match woof_d::OpenAiMemoryGenerator::keychain_backed() {
        Ok(generator) => Some(woof_d::spawn_memory_service(
            woof_d::MemoryScheduler::new(
                memory_storage,
                Arc::new(generator),
                Arc::new(woof_d::SystemMemoryClock),
                woof_d::MemoryScheduleConfig::default(),
            )
            .with_storage_mutation_barrier(memory_mutation_barrier)
            .with_generation_gate(memory_generation_gate),
        )),
        Err(_) => {
            tracing::warn!("memory generation skipped because OpenAI initialization failed");
            None
        }
    };
    let server_result = server_task.await;
    supervisor.shutdown().await;
    automation_supervisor.shutdown().await;
    if let Some(supervisor) = memory_supervisor {
        supervisor.shutdown().await;
    }
    // A cancelled async handler or an aborted supervisor cannot cancel a
    // spawn_blocking closure already mutating durable state. Once every source
    // of new work is stopped, acquire the shared barrier to join any detached
    // mutation before releasing the retained listener descriptor.
    let shutdown_mutation_guard = state.storage_mutation_barrier().lock().await;
    drop(listener_guard);
    drop(shutdown_mutation_guard);
    let server_result = server_result?;
    server_result?;
    Ok(())
}

#[cfg(unix)]
fn retain_listener_lock(listener: &impl AsFd) -> std::io::Result<OwnedFd> {
    listener.as_fd().try_clone_to_owned()
}

#[cfg(not(unix))]
fn retain_listener_lock(_listener: &tokio::net::TcpListener) -> std::io::Result<()> {
    Ok(())
}

async fn bind_before_storage<Listener, Startup, Bind, BindFuture, Initialize>(
    address: SocketAddr,
    bind: Bind,
    initialize: Initialize,
) -> Result<(Listener, Startup), Box<dyn std::error::Error>>
where
    Bind: FnOnce(SocketAddr) -> BindFuture,
    BindFuture: Future<Output = Result<Listener, std::io::Error>>,
    Initialize: FnOnce() -> Result<Startup, Box<dyn std::error::Error>>,
{
    if address != SocketAddr::from(([127, 0, 0, 1], 3334)) {
        return Err("woof_d refuses any listener except 127.0.0.1:3334".into());
    }
    let listener = bind(address).await?;
    let startup = initialize()?;
    Ok((listener, startup))
}

async fn await_local_health(address: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
    let response = tokio::time::timeout(std::time::Duration::from_secs(5), async move {
        let mut stream = tokio::net::TcpStream::connect(address).await?;
        let request =
            format!("GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await?;
        Ok::<_, std::io::Error>(response)
    })
    .await
    .map_err(|_| "woof HTTP server did not answer /health within five seconds")??;

    let separator = b"\r\n\r\n";
    let Some(body_offset) = response
        .windows(separator.len())
        .position(|window| window == separator)
        .map(|offset| offset + separator.len())
    else {
        return Err("woof HTTP server returned an invalid /health response".into());
    };
    let headers = &response[..body_offset - separator.len()];
    let body = &response[body_offset..];
    if !headers.starts_with(b"HTTP/1.1 200 ") || body != br#"{"status":"ok"}"# {
        return Err("woof HTTP server returned an unexpected /health response".into());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DaemonOptions {
    watch_parent_stdin: bool,
    start_paused: bool,
}

fn daemon_options(
    arguments: impl IntoIterator<Item = String>,
) -> Result<DaemonOptions, Box<dyn std::error::Error>> {
    let mut options = DaemonOptions::default();
    for argument in arguments {
        match argument.as_str() {
            "--watch-parent-stdin" => options.watch_parent_stdin = true,
            "--start-paused" => options.start_paused = true,
            _ => return Err(format!("unknown woof_d argument: {argument}").into()),
        }
    }
    Ok(options)
}

async fn shutdown_signal(watch_parent_stdin: bool) {
    let control_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install terminate handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    let parent_closed = async move {
        if !watch_parent_stdin {
            std::future::pending::<()>().await;
        }
        let mut stdin = tokio::io::stdin();
        let mut buffer = [0_u8; 1];
        loop {
            match stdin.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    };

    tokio::select! {
        () = control_c => {},
        () = terminate => {},
        () = parent_closed => {},
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn parent_stdin_watch_is_explicit_and_unknown_arguments_fail_closed() {
        assert_eq!(
            daemon_options(Vec::<String>::new()).unwrap(),
            DaemonOptions {
                watch_parent_stdin: false,
                start_paused: false,
            }
        );
        assert_eq!(
            daemon_options(["--watch-parent-stdin".to_string()]).unwrap(),
            DaemonOptions {
                watch_parent_stdin: true,
                start_paused: false,
            }
        );
        assert_eq!(
            daemon_options(["--start-paused".to_string()]).unwrap(),
            DaemonOptions {
                watch_parent_stdin: false,
                start_paused: true,
            }
        );
        assert!(daemon_options(["--unexpected".to_string()]).is_err());
    }

    #[tokio::test]
    async fn occupied_listener_prevents_storage_initialization() {
        let storage_touched = Cell::new(false);
        let result = bind_before_storage(
            SocketAddr::from(([127, 0, 0, 1], 3334)),
            |_| async {
                Err::<(), _>(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "fixture listener is occupied",
                ))
            },
            || {
                storage_touched.set(true);
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert!(!storage_touched.get());
    }

    #[cfg(unix)]
    #[test]
    fn retained_descriptor_survives_original_descriptor_drop() {
        use std::{
            io::{Read, Write},
            os::unix::net::UnixStream,
        };

        let (descriptor, mut peer) = UnixStream::pair().expect("create socket pair");
        let guard = retain_listener_lock(&descriptor).expect("retain descriptor");
        drop(descriptor);
        let mut retained = UnixStream::from(guard);
        peer.write_all(b"guard").expect("write through peer");
        let mut observed = [0_u8; 5];
        retained
            .read_exact(&mut observed)
            .expect("read through retained descriptor");
        assert_eq!(&observed, b"guard");
    }
}
