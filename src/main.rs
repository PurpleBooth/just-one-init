//! Just One Init
//!
//! Init a process just once inside kubernetes
#![warn(
    rust_2018_idioms,
    unused,
    rust_2021_compatibility,
    nonstandard_style,
    future_incompatible,
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    clippy::unwrap_used,
    clippy::missing_assert_message,
    clippy::todo,
    clippy::allow_attributes_without_reason,
    clippy::panic,
    clippy::panicking_unwrap,
    clippy::panic_in_result_fn
)]

use std::{
    net::SocketAddr,
    option::Option,
    process::{
        Child,
        Command,
        Stdio,
    },
    sync::{
        Arc,
        RwLock,
    },
    time::Duration,
};

use axum::{
    extract::State,
    http::StatusCode,
    response::Json,
    routing::get,
    Extension,
    Router,
    Server,
};
use clap::{
    Parser,
    ValueEnum,
};
use kube_leader_election::{
    LeaseLock,
    LeaseLockParams,
    LeaseLockResult,
};
use miette::{
    miette,
    IntoDiagnostic,
    Result as MietteResult,
};
use serde_json::json;
use tokio::{
    sync,
    sync::mpsc::Sender,
    task::JoinHandle,
};
use tracing::info_span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum LogFormat {
    Json,
    Default,
    Pretty,
    Compact,
}

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// A unique name for the lease that this instance compete for
    #[arg(short, long, env)]
    lease_name: String,

    /// Namespace to use for leader election
    #[arg(short, long, env)]
    pod_namespace: String,

    /// Hostname to use for leader election, this will be used as the name of an instance contending for leadership, and must be unique
    #[arg(short = 'o', long, env)]
    hostname: String,

    /// Hostname to use for leader election, this will be used as the name of an instance contending for leadership, and must be unique
    #[arg(short = 'a', long, env, default_value = "127.0.0.1:5047")]
    listen_addr: String,

    /// TTL for lease, will try to renew at one third this time, so if this is 15, it will try to renew at 5 seconds
    #[arg(short = 't', long, env, default_value = "15")]
    lease_ttl: u64,

    /// Command to run as leader
    command: String,

    /// Arguments to pass to command
    #[arg(last = true)]
    arguments: Vec<String>,

    #[arg(short = 'f', long, env, default_value = "default")]
    log_format: LogFormat,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum AbcState {
    Init,
    Leader,
    Shutdown,
    RenewAttempt,
    Follower,
}

#[tokio::main]
async fn main() -> MietteResult<()> {
    let args = Args::parse();
    o11y(args.log_format);

    let lease_ttl = Duration::from_secs(args.lease_ttl);
    let renew_ttl = lease_ttl / 3;

    let server_listen_addr = args.listen_addr.parse::<SocketAddr>().into_diagnostic()?;
    let current_state = Arc::new(RwLock::from(AbcState::Init));
    let (mptx, mut mprx) = sync::mpsc::channel(10);
    let mut join_handles = Vec::new();

    let leadership = LeaseLock::new(
        kube::Client::try_default().await.into_diagnostic()?,
        &args.pod_namespace,
        LeaseLockParams {
            holder_id: args.hostname.clone(),
            lease_name: args.lease_name.clone(),
            lease_ttl,
        },
    );

    let heartbeat_channel = mptx.clone();
    let renew_heartbeat_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(renew_ttl).await;
            heartbeat_channel
                .send(AbcState::RenewAttempt)
                .await
                .expect("Failed to send heartbeat");
        }
    });
    join_handles.push(renew_heartbeat_handle);

    let shutdown_channel = mptx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl-c");
        shutdown_channel
            .send(AbcState::Shutdown)
            .await
            .expect("Failed to send");
    });

    join_handles.push(start_status_server(
        server_listen_addr,
        current_state.clone(),
    ));

    let mut spawned_process: Option<Child> = None;

    let mptx = mptx.clone();
    loop {
        let mprx_value = mprx.recv().await;

        if let Some(abc_state) = mprx_value {
            if abc_state == AbcState::Leader || abc_state == AbcState::Follower {
                let mut w = current_state.write().expect("Failed to get write lock");
                *w = abc_state;
            }
        };

        match mprx_value {
            Some(AbcState::Init) => {
                let span = info_span!("tick.init", hostname = %args.hostname);
                let _span_guard = span.enter();
                get_lease(mptx.clone(), &leadership).await?;
            }
            Some(AbcState::RenewAttempt) => {
                let span = info_span!("tick.renew_attempt", hostname = %args.hostname);
                let _span_guard = span.enter();
                get_lease(mptx.clone(), &leadership).await?;
            }
            Some(AbcState::Follower) => {
                let span = info_span!("tick.follower", hostname = %args.hostname);
                let _span_guard = span.enter();
                if let Some(ref mut child) = spawned_process {
                    child.kill().into_diagnostic()?;
                    spawned_process = None;
                }
            }
            Some(AbcState::Shutdown) => {
                let span = info_span!("tick.shutdown", hostname = %args.hostname);
                let _span_guard = span.enter();
                shutdown(
                    current_state,
                    &mut join_handles,
                    leadership,
                    &mut spawned_process,
                )
                .await?;

                break;
            }
            Some(AbcState::Leader) => {
                let span = info_span!("tick.leader", hostname = %args.hostname);
                let _span_guard = span.enter();

                let running = leader(&args, &mut spawned_process)?;

                if !running {
                    mptx.send(AbcState::Shutdown).await.expect("Failed to send");
                }
            }
            None => {
                let span = info_span!("tick.nop", hostname = %args.hostname);
                let _span_guard = span.enter();
                tokio::time::sleep(renew_ttl).await;
            }
        }
    }

    Ok(())
}

fn o11y(format: LogFormat) {
    match format {
        LogFormat::Json => {
            tracing_subscriber::fmt()
                .event_format(tracing_subscriber::fmt::format().json())
                .init();
        }
        LogFormat::Pretty => {
            tracing_subscriber::fmt()
                .event_format(tracing_subscriber::fmt::format().pretty())
                .init();
        }
        LogFormat::Compact => {
            tracing_subscriber::fmt()
                .event_format(tracing_subscriber::fmt::format().compact())
                .init();
        }
        LogFormat::Default => {
            tracing_subscriber::fmt().init();
        }
    }
    miette::set_panic_hook();
}

fn leader(args: &Args, spawned_process: &mut Option<Child>) -> MietteResult<bool> {
    match spawned_process {
        None => {
            let child = spawn_process(args.command.clone(), &args.arguments)?;
            *spawned_process = Some(child);
            Ok(true)
        }
        Some(ref mut child) => child.wait().map_or_else(
            |_| Ok(true),
            |status| {
                if status.success() {
                    Ok(false)
                } else {
                    tracing::error!("Subprocess exited with error: {}", status);
                    Err(miette!("Subprocess exited with error: {}", status))
                }
            },
        ),
    }
}

async fn shutdown(
    current_state: Arc<RwLock<AbcState>>,
    join_handles: &mut Vec<JoinHandle<()>>,
    leadership: LeaseLock,
    spawned_process: &mut Option<Child>,
) -> MietteResult<()> {
    if let Some(ref mut child) = spawned_process {
        let _ = child.kill();
        let _ = child.wait();
    }

    if *current_state.read().expect("Failed to get read lock") == AbcState::Leader {
        leadership.step_down().await.into_diagnostic()?;
    }

    for join_handle in join_handles {
        join_handle.abort();
    }
    Ok(())
}

fn spawn_process(command: String, arguments: &[String]) -> MietteResult<Child> {
    tracing::info!("Spawning process: {} {:?}", command, arguments);
    arguments
        .iter()
        .fold(Command::new(command), |mut command, arg| {
            command.arg(arg);
            command
        })
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .into_diagnostic()
}

async fn get_lease(tx: Sender<AbcState>, leadership: &LeaseLock) -> MietteResult<()> {
    let lease_lock_result = leadership.try_acquire_or_renew().await;
    match lease_lock_result {
        Ok(LeaseLockResult {
            acquired_lease: true,
            lease: Some(lease),
        }) => {
            tracing::trace!("Acquired lease {:?}", lease);
            tx.send(AbcState::Leader).await.into_diagnostic()?;
        }
        Ok(
            LeaseLockResult {
                acquired_lease: _,
                lease: None,
            }
            | LeaseLockResult {
                acquired_lease: false,
                lease: _,
            },
        ) => {
            tx.send(AbcState::Follower).await.into_diagnostic()?;
        }
        Err(err) => {
            tx.send(AbcState::Follower).await.into_diagnostic()?;
            tracing::warn!("Failed to acquire lease, continuing: {}", err);
        }
    };
    Ok(())
}

fn start_status_server(
    server_listen_addr: SocketAddr,
    current_state: Arc<RwLock<AbcState>>,
) -> JoinHandle<()> {
    let app = Router::new()
        .layer(Extension(current_state.clone()))
        .route(
            "/",
            get(
                move |State(is_leader): State<Arc<RwLock<AbcState>>>| async move {
                    let state = *(is_leader.read().expect("Failed to read state"));
                    match state {
                        AbcState::Follower => (
                            StatusCode::NOT_FOUND,
                            Json(json!({"status": "ok", "state": "follower"})),
                        ),
                        AbcState::Leader => (
                            StatusCode::OK,
                            Json(json!({"status": "ok", "state": "leader"})),
                        ),
                        _ => (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"status": "error", "state": "unknown"})),
                        ),
                    }
                },
            ),
        )
        .with_state(current_state);

    tokio::spawn(async move {
        Server::bind(&server_listen_addr)
            .serve(app.into_make_service())
            .await
            .expect("Failed to start server");
    })
}
