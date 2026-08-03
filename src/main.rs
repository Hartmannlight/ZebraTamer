mod api;
mod config;
mod mdns;
mod metrics;
mod model;
mod parser;
mod persist;
mod settings;
mod transport;
mod worker;
use anyhow::{Context, Result};
use clap::Parser;
use config::Config;
use model::{now, AccountingConfidence, MediaLedgerEvent};
use persist::Store;
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(version, about)]
struct Args {
    #[arg(
        long,
        env = "ZPL_AGENT_CONFIG",
        default_value = "/etc/zpl-agent/config.toml"
    )]
    config: PathBuf,
    #[arg(long)]
    check_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("zpl_agent=info,tower_http=info")),
        )
        .init();
    let args = Args::parse();
    let config = Config::load(&args.config)?;
    if args.check_config {
        println!("configuration valid");
        return Ok(());
    }
    let store = Store::open(config.data_dir.clone())?;
    apply_boot_accounting(&config, &store)?;
    let recovered = store.recover_jobs()?;
    let mut queues: HashMap<String, Vec<uuid::Uuid>> = HashMap::new();
    for job in recovered {
        queues.entry(job.printer_id).or_default().push(job.id);
    }
    for p in &config.printers {
        store.save_queue(&p.id, queues.get(&p.id).map(Vec::as_slice).unwrap_or(&[]))?;
    }
    let mut workers = HashMap::new();
    for printer in config.printers.clone() {
        workers.insert(
            printer.id.clone(),
            worker::spawn(
                printer,
                store.clone(),
                config.storage_mode,
                config.reconnect_debounce_secs,
                config.printer_reconnect_loss_labels,
                config.hardware_counters,
            ),
        );
    }
    let workers = Arc::new(workers);
    let config = Arc::new(config);
    for w in workers.values() {
        let w = w.clone();
        tokio::spawn(async move {
            let _ = w.probe().await;
        });
    }
    {
        let workers = workers.clone();
        let interval = config.poll_interval_secs;
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(interval.max(1)));
            timer.tick().await;
            loop {
                timer.tick().await;
                for w in workers.values() {
                    let w = w.clone();
                    tokio::spawn(async move {
                        let _ = w.poll_status().await;
                    });
                }
            }
        });
    }
    {
        let workers = workers.clone();
        let interval = config.capability_poll_interval_secs;
        tokio::spawn(async move {
            let mut timer = tokio::time::interval(std::time::Duration::from_secs(interval.max(1)));
            timer.tick().await;
            loop {
                timer.tick().await;
                for w in workers.values() {
                    let w = w.clone();
                    tokio::spawn(async move {
                        let _ = w.probe().await;
                    });
                }
            }
        });
    }
    let _mdns = mdns::announce(&config).context("registering DNS-SD services")?;
    let state = api::AppState {
        config: config.clone(),
        store,
        workers,
        started: now(),
    };
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(address=%config.listen,"zpl-agent listening");
    axum::serve(listener, api::router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}
async fn shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl-C handler")
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {_=ctrl_c=>{},_=terminate=>{}}
}

fn apply_boot_accounting(config: &Config, store: &Store) -> Result<()> {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .unwrap_or_else(|_| "unknown".into());
    let previous: Option<String> = crate::persist::read_json(&store.boot_id_path()).ok();
    if previous.as_deref() != Some(boot.trim()) {
        if previous.is_some() && config.host_boot_loss_labels > 0 {
            for p in &config.printers {
                if let Some(mut m) = store.load_media(&p.id)? {
                    let count = config.host_boot_loss_labels;
                    let before = m.remaining_labels;
                    m.remaining_labels = m.remaining_labels.saturating_sub(count);
                    m.consumed_labels_total = m.consumed_labels_total.saturating_add(count);
                    m.accounting_deficit_labels = m
                        .accounting_deficit_labels
                        .saturating_add(count.saturating_sub(before));
                    m.accounting_confidence = AccountingConfidence::Degraded;
                    m.ledger_sequence += 1;
                    m.last_accounting_event_at = Some(now());
                    store.append_media_ledger(
                        &p.id,
                        &MediaLedgerEvent {
                            sequence: m.ledger_sequence,
                            at: now(),
                            delta: -(count as i64),
                            reason: "host_boot_loss".into(),
                            source: "configured_estimate".into(),
                            job_id: None,
                            before,
                            after: m.remaining_labels,
                            deficit_after: m.accounting_deficit_labels,
                            hardware_counter_before: None,
                            hardware_counter_after: None,
                        },
                    )?;
                    store.save_media(&p.id, &m)?;
                }
            }
        }
        crate::persist::atomic_json(&store.boot_id_path(), &boot.trim())?;
    }
    Ok(())
}
