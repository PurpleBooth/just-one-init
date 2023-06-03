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
    missing_docs
)]

use std::{
    borrow::BorrowMut,
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

use clap::Parser;
use kube_leader_election::{
    LeaseLock,
    LeaseLockParams,
};
use miette::IntoDiagnostic;

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

    let lease_ttl = Duration::from_secs(15);
    let renew_ttl = lease_ttl / 3;

    let is_leader = Arc::new(AtomicBool::new(false));
    {
        let is_leader = is_leader.clone();

        tokio::spawn(async move {
            let leadership = LeaseLock::new(
                kube::Client::try_default()
                    .await
                    .expect("Failed to create client"),
                &args.pod_namespace,
                LeaseLockParams {
                    holder_id: args.hostname.clone(),
                    lease_name: args.lease_name.clone(),
                    lease_ttl,
                },
            );

            loop {
                let leadership_lease = leadership
                    .try_acquire_or_renew()
                    .await
                    .expect("Failed to acquire lease");
                is_leader.store(leadership_lease.acquired_lease, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    let mut spawned_process: Option<std::process::Child> = None;

    loop {
        match (
            is_leader.load(Ordering::Relaxed),
            spawned_process.borrow_mut(),
        ) {
            (false, None) => {
                tracing::info!("not leader");
                spawned_process = None;
            }
            (false, Some(ref mut process)) => {
                tracing::info!("not leader");
                process.kill().into_diagnostic()?;
                spawned_process = None;
            }
            (true, None) => {
                tracing::info!("leader");
                tracing::info!("starting process");
                let child = args
                    .arguments
                    .iter()
                    .fold(Command::new(args.command.clone()), |mut command, arg| {
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
                tracing::info!("leader");

                match process.try_wait().into_diagnostic()? {
                    None => {
                        tracing::info!("process still running");
                    }
                    Some(_) => {
                        tracing::info!("process exited");
                        tracing::info!("goodbye");
                        return Ok(());
                    }
                }
            }
        }
        tokio::time::sleep(renew_ttl).await;
    }
}
