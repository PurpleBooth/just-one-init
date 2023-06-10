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
mod status_server;

use std::{
    fmt::Display,
    net::SocketAddr,
    option::Option,
    sync::Arc,
    time::Duration,
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
use tokio::{
    sync,
    sync::{
        mpsc::Sender,
        RwLock as TokioRwLock,
    },
};
use tracing::{
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
    #[arg(short = 'a', long, env, default_value = "[::]:5047")]
    listen_addr: String,

    /// TTL for lease, will try to renew at one third this time, so if this is 15, it will try to renew at 5 seconds
    #[arg(short = 't', long, env, default_value = "15")]
    lease_ttl: u64,

    /// Command to run
    #[arg(last = true)]
    command: Vec<String>,
}

/// Events that the actor handles
#[derive(Debug, Eq, PartialEq, Clone, Copy)]
pub enum JustOneInitState {
    /// Actor is leader and should run process
    BecameLeader,
    /// Actor is shutting down
    BeganShutdown,
    /// Actor is follower and should not run process
    BecameFollower,
}

impl Display for JustOneInitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BecameLeader => write!(f, "BecameLeader"),
            Self::BeganShutdown => write!(f, "BeganShutdown"),
            Self::BecameFollower => write!(f, "BecameFollower"),
        }
    }
}

#[instrument]
#[tokio::main]
async fn main() -> MietteResult<()> {
    let args = Args::parse();
    o11y()?;

    let lease_ttl = Duration::from_secs(args.lease_ttl);
    let renew_ttl = lease_ttl / 3;

    let server_listen_addr = args.listen_addr.parse::<SocketAddr>().into_diagnostic()?;
    let state = Arc::new(TokioRwLock::from(JustOneInitState::BecameFollower));
    let (event_sender, mut event_receiver) = sync::mpsc::channel(10);
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

    let ctrl_c_event_sender = event_sender.clone();
    let ctrl_c = async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl-c");
        ctrl_c_event_sender
            .send(JustOneInitState::BeganShutdown)
            .await
            .expect("Failed to send()");
    };
    tokio::spawn(ctrl_c.instrument(tracing::info_span!("ctrl_c")));

    join_handles.push(status_server::spawn(server_listen_addr, state.clone()));

    let mut process_manager = ProcessManager::from(args.command);
    event_sender
        .send(JustOneInitState::BecameFollower)
        .await
        .into_diagnostic()?;

    loop {
        match event_receiver.try_recv().ok() {
            Some(JustOneInitState::BecameFollower) => {
                *state.write().await = JustOneInitState::BecameFollower;
                process_manager.stop()?;
            }
            Some(JustOneInitState::BeganShutdown) => {
                process_manager.stop()?;

                if *state.read().await == JustOneInitState::BecameLeader {
                    leadership.step_down().await.into_diagnostic()?;
                }

                for join_handle in &mut join_handles {
                    join_handle.abort();
                }

                if process_manager.check_if_exit_successful() == Some(false) {
                    return Err(miette!("Process exited with non-zero exit code"));
                }

                break;
            }
            Some(JustOneInitState::BecameLeader) => {
                *state.write().await = JustOneInitState::BecameLeader;
                process_manager.start()?;

                if !process_manager.check_if_running() {
                    event_sender
                        .send(JustOneInitState::BeganShutdown)
                        .await
                        .into_diagnostic()?;
                }
            }
            None => {
                get_lease(event_sender.clone(), &leadership).await?;
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
async fn get_lease(tx: Sender<JustOneInitState>, leadership: &LeaseLock) -> MietteResult<()> {
    let lease_lock_result = leadership.try_acquire_or_renew().await;
    match lease_lock_result {
        Ok(LeaseLockResult {
            acquired_lease: true,
            lease: Some(_),
        }) => {
            tx.send(JustOneInitState::BecameLeader)
                .await
                .into_diagnostic()?;
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
            tx.send(JustOneInitState::BecameFollower)
                .await
                .into_diagnostic()?;
        }
        Err(err) => {
            tx.send(JustOneInitState::BecameFollower)
                .await
                .into_diagnostic()?;
            warn!("Failed to acquire lease, continuing: {:?}", err);
        }
    };
    Ok(())
}
