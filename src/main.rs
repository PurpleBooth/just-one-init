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

mod process_launcher;

use std::{
    net::SocketAddr,
    option::Option,
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
use clap::Parser;
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
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer,
    trace::TraceLayer,
};
use tracing::{
    event,
    instrument,
    warn,
    Instrument,
};
use tracing_subscriber::{
    fmt,
    prelude::*,
    EnvFilter,
};

use crate::process_launcher::ProcessManager;

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

    /// Command to run
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Debug, Eq, PartialEq, Clone, Copy)]
enum JustOneInitState {
    BeganInit,
    BecameLeader,
    BeganShutdown,
    BeganRenewAttempt,
    BecameFollower,
}

#[instrument]
#[tokio::main]
async fn main() -> MietteResult<()> {
    let args = Args::parse();
    o11y()?;

    let lease_ttl = Duration::from_secs(args.lease_ttl);
    let renew_ttl = lease_ttl / 3;

    let server_listen_addr = args.listen_addr.parse::<SocketAddr>().into_diagnostic()?;
    let current_state = Arc::new(RwLock::from(JustOneInitState::BeganInit));
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
                .send(JustOneInitState::BeganRenewAttempt)
                .await
                .expect("Failed to send heartbeat");
        }
    });
    join_handles.push(renew_heartbeat_handle);

    let shutdown_channel = mptx.clone();
    let ctrl_c = async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl-c");
        shutdown_channel
            .send(JustOneInitState::BeganShutdown)
            .await
            .expect("Failed to send");
    };
    tokio::spawn(ctrl_c.instrument(tracing::info_span!("ctrl_c")));

    join_handles.push(start_status_server(
        server_listen_addr,
        current_state.clone(),
    ));

    let mut spawned_process = ProcessManager::from(args.command);

    let mptx = mptx.clone();
    loop {
        let mprx_value = mprx.recv().await;

        if let Some(abc_state) = mprx_value {
            event!(tracing::Level::INFO, "{:?}", abc_state);

            if abc_state == JustOneInitState::BecameLeader || abc_state == JustOneInitState::BecameFollower {
                let mut w = current_state.write().expect("Failed to get write lock");
                *w = abc_state;
            }
        };

        match mprx_value {
            Some(JustOneInitState::BeganInit) => {
                get_lease(mptx.clone(), &leadership).await?;
            }
            Some(JustOneInitState::BeganRenewAttempt) => {
                get_lease(mptx.clone(), &leadership).await?;
            }
            Some(JustOneInitState::BecameFollower) => {
                spawned_process.stop()?;
            }
            Some(JustOneInitState::BeganShutdown) => {
                spawned_process.stop()?;
                shutdown(current_state, &mut join_handles, leadership).await?;

                if spawned_process.check_if_exit_successful() == Some(false) {
                    return Err(miette!("Process exited with non-zero exit code"));
                }

                break;
            }
            Some(JustOneInitState::BecameLeader) => {
                spawned_process.start()?;

                if !spawned_process.check_if_running() {
                    mptx.send(JustOneInitState::BeganShutdown)
                        .await
                        .expect("Failed to send");
                }
            }
            None => {
                tokio::time::sleep(renew_ttl).await;
            }
        }
    }

    Ok(())
}

fn o11y() -> MietteResult<()> {
    miette::set_panic_hook();

    let fmt_layer = fmt::layer();
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("info"))
        .into_diagnostic()?;

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    Ok(())
}

#[instrument(skip(leadership))]
async fn shutdown(
    current_state: Arc<RwLock<JustOneInitState>>,
    join_handles: &mut Vec<JoinHandle<()>>,
    leadership: LeaseLock,
) -> MietteResult<()> {
    if *current_state.read().expect("Failed to get read lock") == JustOneInitState::BecameLeader {
        leadership.step_down().await.into_diagnostic()?;
    }

    for join_handle in join_handles {
        join_handle.abort();
    }
    Ok(())
}

#[instrument(skip(leadership))]
async fn get_lease(tx: Sender<JustOneInitState>, leadership: &LeaseLock) -> MietteResult<()> {
    let lease_lock_result = leadership.try_acquire_or_renew().await;
    match lease_lock_result {
        Ok(LeaseLockResult {
            acquired_lease: true,
            lease: Some(_),
        }) => {
            tx.send(JustOneInitState::BecameLeader).await.into_diagnostic()?;
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
            tx.send(JustOneInitState::BecameFollower).await.into_diagnostic()?;
        }
        Err(err) => {
            tx.send(JustOneInitState::BecameFollower).await.into_diagnostic()?;
            warn!("Failed to acquire lease, continuing: {:?}", err);
        }
    };
    Ok(())
}

#[instrument]
fn start_status_server(
    server_listen_addr: SocketAddr,
    current_state: Arc<RwLock<JustOneInitState>>,
) -> JoinHandle<()> {
    let app = Router::new()
        .layer(
            ServiceBuilder::new()
                .layer(TraceLayer::new_for_http())
                .layer(CompressionLayer::new()),
        )
        .layer(Extension(current_state.clone()))
        .route(
            "/",
            get(
                move |State(is_leader): State<Arc<RwLock<JustOneInitState>>>| async move {
                    let state = *(is_leader.read().expect("Failed to read state"));
                    match state {
                        JustOneInitState::BecameFollower => (
                            StatusCode::NOT_FOUND,
                            Json(json!({"status": "ok", "state": "follower"})),
                        ),
                        JustOneInitState::BecameLeader => (
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

    let start_http_server = async move {
        Server::bind(&server_listen_addr)
            .serve(app.into_make_service())
            .await
            .expect("Failed to start server");
    };
    tokio::spawn(start_http_server.instrument(tracing::info_span!("http_server")))
}
