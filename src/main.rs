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
    borrow::BorrowMut,
    net::SocketAddr,
    option::Option,
    process::{
        Command,
        Stdio,
    },
    sync::{
        atomic::{
            AtomicBool,
            Ordering,
        },
        Arc,
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
};
use miette::IntoDiagnostic;
use serde_json::json;

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Namespace to use for leader election
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
}

#[tokio::main]
async fn main() -> miette::Result<()> {
    tracing_subscriber::fmt::init();
    miette::set_panic_hook();
    let args = Args::parse();

    let lease_ttl = Duration::from_secs(args.lease_ttl);
    let renew_ttl = lease_ttl / 3;

    let server_listen_addr = args.listen_addr.parse::<SocketAddr>().into_diagnostic()?;

    let is_leader = Arc::new(AtomicBool::new(false));
    elect_leader(
        args.pod_namespace,
        args.hostname,
        args.lease_name,
        lease_ttl,
        is_leader.clone(),
    );
    start_status_server(server_listen_addr, is_leader.clone());
    process_starter(args.command, args.arguments, renew_ttl, is_leader.clone()).await?;

    Ok(())
}

fn elect_leader(
    pod_namespace: String,
    hostname: String,
    lease_name: String,
    lease_ttl: Duration,
    is_leader: Arc<AtomicBool>,
) {
    {
        let is_leader = is_leader;

        tokio::spawn(async move {
            let leadership = LeaseLock::new(
                kube::Client::try_default()
                    .await
                    .expect("Failed to create client"),
                &pod_namespace,
                LeaseLockParams {
                    holder_id: hostname.clone(),
                    lease_name: lease_name.clone(),
                    lease_ttl,
                },
            );

            loop {
                let mut lease_result = leadership.try_acquire_or_renew().await;

                if let Err(error) = lease_result {
                    tracing::warn!("Failed to acquire lease, retrying: {}", error);
                    lease_result = leadership.try_acquire_or_renew().await;
                }

                let leadership_lease = lease_result.expect("Failed to acquire lease");
                is_leader.store(leadership_lease.acquired_lease, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

fn start_status_server(server_listen_addr: SocketAddr, is_leader: Arc<AtomicBool>) {
    let app = Router::new()
        .layer(Extension(is_leader.clone()))
        .route(
            "/",
            get(move |State(is_leader): State<Arc<AtomicBool>>| async move {
                if is_leader.load(Ordering::Relaxed) {
                    (
                        StatusCode::OK,
                        Json(json!({"status": "ok", "leader": true})),
                    )
                } else {
                    (
                        StatusCode::NOT_FOUND,
                        Json(json!({"status": "ok", "leader": false})),
                    )
                }
            }),
        )
        .with_state(is_leader);

    tokio::spawn(async move {
        Server::bind(&server_listen_addr)
            .serve(app.into_make_service())
            .await
            .expect("Failed to start server");
    });
}

async fn process_starter(
    command: String,
    arguments: Vec<String>,
    renew_ttl: Duration,
    is_leader: Arc<AtomicBool>,
) -> miette::Result<()> {
    let mut spawned_process: Option<std::process::Child> = None;

    loop {
        match (
            is_leader.load(Ordering::Relaxed),
            spawned_process.borrow_mut(),
        ) {
            (false, None) => {
                tracing::trace!("leader: false, process: unspawned");
                spawned_process = None;
            }
            (false, Some(ref mut process)) => {
                tracing::trace!("leader: false, process: killing");
                process.kill().into_diagnostic()?;
                spawned_process = None;
            }
            (true, None) => {
                tracing::trace!("leader: true, process: unspawned");
                let child = arguments
                    .iter()
                    .fold(Command::new(command.clone()), |mut command, arg| {
                        command.arg(arg);
                        command
                    })
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .into_diagnostic()?;

                spawned_process = Some(child);
            }
            (true, Some(ref mut process)) => {
                tracing::info!("leader: true");
                match process.try_wait().into_diagnostic()? {
                    None => {
                        tracing::trace!("leader: true, process: spawned");
                    }
                    Some(_) => {
                        tracing::trace!("leader: true, process: exited");
                        return Ok(());
                    }
                }
            }
        }
        tokio::time::sleep(renew_ttl).await;
    }
}
