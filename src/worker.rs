use crate::{
    config::{HardwareCounterPolicy, PrinterConfig, StorageMode},
    model::{now, JobState, Observed, PrinterSnapshot, RawResponse},
    persist::Store,
    transport::{CharDeviceTransport, PrinterTransport},
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;
use std::{
    collections::VecDeque,
    fs,
    sync::{
        mpsc::{self, Receiver, SyncSender},
        Arc, RwLock,
    },
    thread,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use uuid::Uuid;

pub const STATUS_QUERIES: &[(&str, &[u8])] = &[("host_status", b"~HS")];
pub const INVENTORY_QUERIES: &[(&str, &[u8])] = &[
    ("identification", b"~HI"),
    ("head_diagnostics", b"~HD"),
    ("memory", b"~HM"),
    ("battery", b"~HB"),
    ("configuration", b"^XA^HH^XZ"),
    ("hq_errors", b"~HQES"),
    ("hq_head_test", b"~HQJT"),
    ("hq_maintenance", b"~HQMA"),
    ("hq_odometer", b"~HQOD"),
    ("hq_head_life", b"~HQPH"),
    ("hq_plug_and_play", b"~HQPP"),
    ("hq_serial", b"~HQSN"),
    ("hq_usb", b"~HQUI"),
    ("xml_status", b"^XA^HZr^XZ"),
    (
        "sgd_odometer",
        b"! U1 getvar \"odometer.total_print_length\"\r\n",
    ),
];

enum Command {
    Probe {
        capabilities: bool,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Enqueue(Uuid),
}

#[derive(Clone)]
pub struct WorkerHandle {
    tx: SyncSender<Command>,
    pub snapshot: Arc<RwLock<PrinterSnapshot>>,
    pub config: PrinterConfig,
}

#[derive(Clone, Copy)]
struct WorkerOptions {
    storage: StorageMode,
    reconnect_debounce_secs: u64,
    reconnect_loss_labels: u64,
    hardware_counters: HardwareCounterPolicy,
}

impl WorkerHandle {
    pub async fn probe(&self) -> Result<(), String> {
        self.request_probe(true).await
    }
    pub async fn poll_status(&self) -> Result<(), String> {
        self.request_probe(false).await
    }
    async fn request_probe(&self, capabilities: bool) -> Result<(), String> {
        if !self.config.bidirectional_queries {
            return Err("bidirectional_queries_disabled".to_string());
        }
        let (tx, rx) = oneshot::channel();
        self.tx
            .try_send(Command::Probe {
                capabilities,
                reply: tx,
            })
            .map_err(|_| "worker_queue_full".to_string())?;
        rx.await.map_err(|_| "worker_stopped".to_string())?
    }
    pub fn enqueue(&self, id: Uuid) -> Result<(), String> {
        self.tx
            .try_send(Command::Enqueue(id))
            .map_err(|_| "worker_queue_full".to_string())
    }
}

pub fn spawn(
    config: PrinterConfig,
    store: Store,
    storage: StorageMode,
    reconnect_debounce_secs: u64,
    reconnect_loss_labels: u64,
    hardware_counters: HardwareCounterPolicy,
) -> WorkerHandle {
    let options = WorkerOptions {
        storage,
        reconnect_debounce_secs,
        reconnect_loss_labels,
        hardware_counters,
    };
    let mut initial_snapshot = PrinterSnapshot::new(config.id.clone());
    if let Some(model) = config.model_hint.clone() {
        initial_snapshot.identity.model = Observed::value(model, None, "config_hint");
    }
    initial_snapshot.transport.present = Observed::value(config.device.exists(), None, "os");
    if !config.bidirectional_queries {
        initial_snapshot.transport.protocol_up =
            Observed::unavailable("bidirectional_queries_disabled");
        initial_snapshot.status.ready = Observed::unavailable("bidirectional_queries_disabled");
    }
    let snapshot_tx = Arc::new(RwLock::new(initial_snapshot));
    let shared = snapshot_tx.clone();
    let (thread_cfg, handle_cfg) = (config.clone(), config.clone());
    let (tx, rx) = mpsc::sync_channel(64);
    thread::Builder::new()
        .name(format!("printer-{}", config.id))
        .spawn(move || run(thread_cfg, store, options, shared, rx))
        .expect("spawn printer worker");
    WorkerHandle {
        tx,
        snapshot: snapshot_tx,
        config: handle_cfg,
    }
}

fn run(
    config: PrinterConfig,
    store: Store,
    options: WorkerOptions,
    snapshot: Arc<RwLock<PrinterSnapshot>>,
    rx: Receiver<Command>,
) {
    let mut queue: VecDeque<Uuid> = store.load_queue(&config.id).unwrap_or_default().into();
    let mut transport: Option<CharDeviceTransport> = None;
    let mut ever_connected = false;
    let mut disconnected_at: Option<std::time::Instant> = None;
    loop {
        if let Some(id) = queue.pop_front() {
            let _ = store.save_queue(&config.id, queue.make_contiguous());
            snapshot.write().unwrap().jobs.queue_depth = queue.len() as u64 + 1;
            process_job(
                id,
                &config,
                &store,
                options.storage,
                &snapshot,
                &mut transport,
            );
            snapshot.write().unwrap().jobs.queue_depth = queue.len() as u64;
            continue;
        }
        match rx.recv() {
            Ok(Command::Probe {
                capabilities,
                reply,
            }) => {
                let result = probe(
                    &config,
                    &store,
                    &snapshot,
                    &mut transport,
                    capabilities,
                    options.hardware_counters,
                );
                if result.is_ok() {
                    if ever_connected
                        && disconnected_at
                            .map(|at| {
                                at.elapsed() >= Duration::from_secs(options.reconnect_debounce_secs)
                            })
                            .unwrap_or(false)
                    {
                        apply_reconnect_loss(&config, &store, options.reconnect_loss_labels);
                    }
                    ever_connected = true;
                    disconnected_at = None;
                } else if ever_connected {
                    disconnected_at.get_or_insert_with(std::time::Instant::now);
                    transport = None;
                }
                let _ = reply.send(result);
            }
            Ok(Command::Enqueue(id)) => {
                queue.push_back(id);
                let _ = store.save_queue(&config.id, queue.make_contiguous());
                snapshot.write().unwrap().jobs.queue_depth = queue.len() as u64;
            }
            Err(_) => break,
        }
    }
}

fn apply_reconnect_loss(config: &PrinterConfig, store: &Store, count: u64) {
    if count == 0 {
        return;
    }
    if let Ok(Some(mut media)) = store.load_media(&config.id) {
        let before = media.remaining_labels;
        media.remaining_labels = media.remaining_labels.saturating_sub(count);
        media.consumed_labels_total = media.consumed_labels_total.saturating_add(count);
        media.accounting_deficit_labels = media
            .accounting_deficit_labels
            .saturating_add(count.saturating_sub(before));
        media.accounting_confidence = crate::model::AccountingConfidence::Degraded;
        media.ledger_sequence += 1;
        media.last_accounting_event_at = Some(now());
        let event = crate::model::MediaLedgerEvent {
            sequence: media.ledger_sequence,
            at: now(),
            delta: -(count as i64),
            reason: "printer_reconnect_loss".into(),
            source: "configured_estimate".into(),
            job_id: None,
            before,
            after: media.remaining_labels,
            deficit_after: media.accounting_deficit_labels,
            hardware_counter_before: None,
            hardware_counter_after: None,
        };
        let _ = store.append_media_ledger(&config.id, &event);
        let _ = store.save_media(&config.id, &media);
    }
}

fn ensure_transport(
    config: &PrinterConfig,
    transport: &mut Option<CharDeviceTransport>,
    snapshot: &Arc<RwLock<PrinterSnapshot>>,
) -> Result<(), String> {
    if transport.is_none() {
        match CharDeviceTransport::open(config) {
            Ok(t) => {
                *transport = Some(t);
                let mut s = snapshot.write().unwrap();
                s.transport.present = Observed::value(true, None, "os");
                s.transport.open = Observed::value(true, None, "agent");
                s.transport.connected_at = Observed::value(now(), None, "agent");
            }
            Err(e) => {
                let mut s = snapshot.write().unwrap();
                s.transport.present = Observed::value(config.device.exists(), None, "os");
                s.transport.open = Observed::value(false, None, "agent");
                s.transport.protocol_up = Observed::value(false, None, "probe");
                s.status.ready = Observed::unavailable(e.to_string());
                return Err(e.to_string());
            }
        }
    }
    Ok(())
}

fn probe(
    config: &PrinterConfig,
    store: &Store,
    snapshot: &Arc<RwLock<PrinterSnapshot>>,
    transport: &mut Option<CharDeviceTransport>,
    capability_probe: bool,
    hardware_counters: HardwareCounterPolicy,
) -> Result<(), String> {
    ensure_transport(config, transport, snapshot)?;
    let t = transport.as_mut().unwrap();
    let mut any = false;
    for (name, command) in STATUS_QUERIES {
        let started = Instant::now();
        match t.query(command, config.first_byte_timeout(), config.idle_timeout()) {
            Ok(r) if !r.bytes.is_empty() => {
                any = true;
                record_response(snapshot, name, &r.bytes, r.duration, None, r.classification);
                parse_known(name, &r.bytes, snapshot);
            }
            Ok(r) => record_response(snapshot, name, &r.bytes, r.duration, None, r.classification),
            Err(e) => record_response(
                snapshot,
                name,
                &[],
                started.elapsed(),
                Some(e.to_string()),
                "transport_error".into(),
            ),
        }
    }
    // Some Zebra models suppress ~HS in particular fault states. A successful
    // identification response still proves that the bidirectional channel is up.
    if !any {
        let name = "identification";
        let started = Instant::now();
        match t.query(b"~HI", config.first_byte_timeout(), config.idle_timeout()) {
            Ok(r) if !r.bytes.is_empty() => {
                any = true;
                record_response(snapshot, name, &r.bytes, r.duration, None, r.classification);
                parse_known(name, &r.bytes, snapshot);
            }
            Ok(r) => record_response(snapshot, name, &r.bytes, r.duration, None, r.classification),
            Err(error) => record_response(
                snapshot,
                name,
                &[],
                started.elapsed(),
                Some(error.to_string()),
                "transport_error".into(),
            ),
        }
    }
    if capability_probe {
        for (name, command) in INVENTORY_QUERIES {
            if *name == "sgd_odometer" && hardware_counters == HardwareCounterPolicy::Disabled {
                snapshot.write().unwrap().capabilities.insert(
                    (*name).into(),
                    Observed::not_supported("disabled_by_config"),
                );
                continue;
            }
            let started = Instant::now();
            match t.query(command, config.first_byte_timeout(), config.idle_timeout()) {
                Ok(r) if !r.bytes.is_empty() => {
                    any = true;
                    record_response(snapshot, name, &r.bytes, r.duration, None, r.classification);
                    parse_known(name, &r.bytes, snapshot);
                    snapshot
                        .write()
                        .unwrap()
                        .capabilities
                        .insert((*name).into(), Observed::value(true, None, "probe"));
                }
                Ok(r) => {
                    record_response(snapshot, name, &r.bytes, r.duration, None, r.classification);
                    snapshot
                        .write()
                        .unwrap()
                        .capabilities
                        .insert((*name).into(), Observed::not_supported("empty_response"));
                }
                Err(e) if e.to_string().contains("timeout") => {
                    record_response(
                        snapshot,
                        name,
                        &[],
                        started.elapsed(),
                        Some(e.to_string()),
                        "transport_error".into(),
                    );
                    snapshot
                        .write()
                        .unwrap()
                        .capabilities
                        .insert((*name).into(), Observed::unavailable("probe_timeout"));
                }
                Err(e) => {
                    let error = e.to_string();
                    record_response(
                        snapshot,
                        name,
                        &[],
                        started.elapsed(),
                        Some(error.clone()),
                        "transport_error".into(),
                    );
                    snapshot
                        .write()
                        .unwrap()
                        .capabilities
                        .insert((*name).into(), Observed::unavailable(error));
                }
            }
        }
    }
    if capability_probe {
        let mut s = snapshot.write().unwrap();
        s.counters.manual_feed_detection =
            match s.capabilities.get("sgd_odometer").map(|v| &v.state) {
                Some(crate::model::ValueState::Value) => {
                    Observed::value(true, None, "capability_probe")
                }
                Some(crate::model::ValueState::NotSupported) => {
                    Observed::not_supported("hardware_counter_not_supported")
                }
                Some(crate::model::ValueState::Unavailable) => {
                    Observed::unavailable("hardware_counter_probe_unavailable")
                }
                _ => Observed::unknown(),
            };
    }
    {
        let mut s = snapshot.write().unwrap();
        s.transport.protocol_up = Observed::value(any, None, "probe");
        if any {
            s.transport.last_response_at = Observed::value(now(), None, "probe");
        } else {
            s.status.ready = Observed::unavailable("printer_did_not_answer_status_query");
            mark_status_stale(&mut s.status);
            s.transport.open = Observed::value(false, None, "agent");
            s.transport.disconnected_at = Observed::value(now(), None, "probe");
        }
        s.updated_at = now();
        let _ =
            crate::persist::atomic_json(&store.printer_dir(&config.id).join("snapshot.json"), &*s);
    }
    if any {
        Ok(())
    } else {
        Err("printer did not answer any base query".into())
    }
}

fn mark_stale<T>(observation: &mut Observed<T>) {
    if observation.value.is_some() {
        observation.state = crate::model::ValueState::Stale;
        observation.reason = Some("printer_offline".into());
    }
}

fn mark_status_stale(status: &mut crate::model::StatusSnapshot) {
    mark_stale(&mut status.ready);
    mark_stale(&mut status.paused);
    mark_stale(&mut status.media_out);
    mark_stale(&mut status.ribbon_out);
    mark_stale(&mut status.head_open);
    mark_stale(&mut status.temperature_fault);
    mark_stale(&mut status.buffer_available_bytes);
    mark_stale(&mut status.buffer_full);
    mark_stale(&mut status.print_mode);
    mark_stale(&mut status.batch_total);
    mark_stale(&mut status.batch_remaining);
    mark_stale(&mut status.formats_buffered);
    mark_stale(&mut status.images_stored);
    mark_stale(&mut status.partial_format);
    mark_stale(&mut status.corrupt_configuration);
    mark_stale(&mut status.cutter_jam);
    mark_stale(&mut status.cover_open);
    mark_stale(&mut status.clean_head_warning);
    mark_stale(&mut status.media_low);
    mark_stale(&mut status.ribbon_low);
}

fn record_response(
    snapshot: &Arc<RwLock<PrinterSnapshot>>,
    name: &str,
    bytes: &[u8],
    duration: Duration,
    error: Option<String>,
    classification: String,
) {
    let mut s = snapshot.write().unwrap();
    let stats = s.query_stats.entry(name.into()).or_default();
    stats.count = stats.count.saturating_add(1);
    stats.duration_ms_total = stats
        .duration_ms_total
        .saturating_add(duration.as_millis() as u64);
    stats.response_bytes_total = stats
        .response_bytes_total
        .saturating_add(bytes.len() as u64);
    if error.as_deref().is_some_and(|e| e.contains("timeout")) {
        stats.timeouts = stats.timeouts.saturating_add(1);
    }
    s.raw_responses.insert(
        name.into(),
        RawResponse {
            text: String::from_utf8_lossy(bytes).into(),
            base64: STANDARD.encode(bytes),
            observed_at: now(),
            duration_ms: duration.as_millis() as u64,
            error,
            classification,
        },
    );
}

fn parse_known(name: &str, bytes: &[u8], snapshot: &Arc<RwLock<PrinterSnapshot>>) {
    let mut s = snapshot.write().unwrap();
    if let Err(error) = crate::parser::parse(name, bytes, &mut s) {
        let stats = s.query_stats.entry(name.into()).or_default();
        stats.parse_errors = stats.parse_errors.saturating_add(1);
        if let Some(raw) = s.raw_responses.get_mut(name) {
            raw.error = Some(format!("parse_error:{error}"));
        }
    }
}

fn process_job(
    id: Uuid,
    config: &PrinterConfig,
    store: &Store,
    storage: StorageMode,
    snapshot: &Arc<RwLock<PrinterSnapshot>>,
    transport: &mut Option<CharDeviceTransport>,
) {
    let Ok(mut job) = store.load_job(id) else {
        return;
    };
    job.state = JobState::Writing;
    job.updated_at = now();
    let _ = store.save_job(&job);
    emit(store, "job_writing", config, id, json!({}));
    // Close a possible read/write query descriptor first. Print jobs use a
    // separate fresh write-only descriptor, matching the raw printer path.
    *transport = None;
    let result = job
        .payload_path
        .as_ref()
        .ok_or("payload_missing".to_string())
        .and_then(|path| CharDeviceTransport::write_file(config, path).map_err(|e| e.to_string()));
    match result {
        Ok(bytes) => {
            job.state = JobState::TransportAccepted;
            job.updated_at = now();
            let _ = store.save_job(&job);
            emit(
                store,
                "job_transport_accepted",
                config,
                id,
                json!({ "bytes": bytes }),
            );
            consume_media(store, config, &job);
            if storage == StorageMode::MetadataOnly {
                if let Some(p) = &job.payload_path {
                    let _ = fs::remove_file(p);
                }
                job.payload_path = None;
            }
        }
        Err(e) => {
            job.state = if job.bytes > 0 {
                JobState::OutcomeUnknown
            } else {
                JobState::Failed
            };
            job.error = Some(e);
            job.updated_at = now();
        }
    }
    let _ = store.save_job(&job);
    let mut s = snapshot.write().unwrap();
    s.jobs.last_job_id = Some(id);
    s.jobs.last_job_state = Some(job.state.clone());
    s.updated_at = now();
}

fn consume_media(store: &Store, config: &PrinterConfig, job: &crate::model::Job) {
    if let (Some(count), Ok(Some(mut media))) = (job.label_count, store.load_media(&config.id)) {
        let before = media.remaining_labels;
        let consumed = count.min(before);
        media.remaining_labels -= consumed;
        media.consumed_labels_total = media.consumed_labels_total.saturating_add(count);
        media.accounting_deficit_labels = media
            .accounting_deficit_labels
            .saturating_add(count - before.min(count));
        media.ledger_sequence += 1;
        media.last_accounting_event_at = Some(now());
        let ev = crate::model::MediaLedgerEvent {
            sequence: media.ledger_sequence,
            at: now(),
            delta: -(count as i64),
            reason: "job".into(),
            source: job
                .label_count_source
                .clone()
                .unwrap_or_else(|| "unknown".into()),
            job_id: Some(job.id),
            before,
            after: media.remaining_labels,
            deficit_after: media.accounting_deficit_labels,
            hardware_counter_before: None,
            hardware_counter_after: None,
        };
        let _ = store.append_media_ledger(&config.id, &ev);
        let _ = store.save_media(&config.id, &media);
    }
}
fn emit(store: &Store, kind: &str, config: &PrinterConfig, id: Uuid, data: serde_json::Value) {
    let e = store.next_event(kind, Some(config.id.clone()), Some(id), data);
    let _ = store.append_event(&e);
}
