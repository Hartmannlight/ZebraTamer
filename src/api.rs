use crate::{
    config::{Config, StorageMode},
    metrics,
    model::{
        now, AccountingConfidence, ApiError, Envelope, Event, Job, JobState, MediaAdjustment,
        MediaDefinition, MediaLedgerEvent, MediaState, Observed,
    },
    persist::Store,
    settings::{PrinterSettings, PrinterSettingsRecord},
    worker::WorkerHandle,
};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Arc};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Store,
    pub workers: Arc<HashMap<String, WorkerHandle>>,
    pub started: chrono::DateTime<chrono::Utc>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/healthz", get(health))
        .route("/metrics", get(prometheus))
        .route("/v1/agent", get(agent))
        .route("/v1/printers", get(printers))
        .route("/v1/printers/{id}", get(printer))
        .route("/v1/printers/{id}/status", get(printer_status))
        .route("/v1/printers/{id}/snapshot", get(snapshot))
        .route("/v1/printers/{id}/capabilities", get(capabilities))
        .route(
            "/v1/printers/{id}/settings",
            get(printer_settings).patch(patch_printer_settings),
        )
        .route("/v1/printers/{id}/probe", post(probe))
        .route("/v1/printers/{id}/jobs", post(create_job))
        .route("/v1/jobs", get(jobs))
        .route("/v1/jobs/{id}", get(job))
        .route("/v1/jobs/{id}/events", get(job_events))
        .route("/v1/jobs/{id}/payload", get(payload))
        .route("/v1/printers/{id}/media", get(media).put(put_media))
        .route("/v1/printers/{id}/media/unload", post(unload_media))
        .route("/v1/printers/{id}/media/adjustments", post(adjust_media))
        .route("/v1/events", get(events))
        .layer(DefaultBodyLimit::disable())
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn root() -> Json<Envelope<Value>> {
    Json(Envelope::ok(
        json!({"service":"zpl-agent","api_version":"v1"}),
    ))
}
async fn health() -> Json<Envelope<Value>> {
    Json(Envelope::ok(json!({"status":"ok"})))
}
async fn agent(State(s): State<AppState>) -> Json<Envelope<Value>> {
    Json(Envelope::ok(
        json!({"version":env!("CARGO_PKG_VERSION"),"commit":env!("ZPL_AGENT_GIT_COMMIT"),"started_at":s.started,"uptime_seconds":(now()-s.started).num_seconds().max(0),"storage_mode":s.config.storage_mode,"printers":s.workers.len()}),
    ))
}
async fn prometheus(State(s): State<AppState>) -> Response {
    let text = metrics::render(&s);
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        text,
    )
        .into_response()
}

#[derive(Serialize)]
struct PrinterItem {
    id: String,
    display_name: String,
    transport: String,
    device: String,
    online: Option<bool>,
    present: Option<bool>,
    status_path: String,
    snapshot_path: String,
}
async fn printers(State(s): State<AppState>) -> Json<Envelope<Vec<PrinterItem>>> {
    Json(Envelope::ok(
        s.config
            .printers
            .iter()
            .map(|p| {
                let snapshot = s
                    .workers
                    .get(&p.id)
                    .map(|worker| worker.snapshot.read().unwrap());
                PrinterItem {
                    id: p.id.clone(),
                    display_name: p.display_name.clone(),
                    transport: p.transport.clone(),
                    device: p.device.display().to_string(),
                    online: snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.transport.protocol_up.value),
                    present: snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.transport.present.value),
                    status_path: format!("/v1/printers/{}/status", p.id),
                    snapshot_path: format!("/v1/printers/{}/snapshot", p.id),
                }
            })
            .collect(),
    ))
}
fn worker<'a>(s: &'a AppState, id: &str) -> Result<&'a WorkerHandle, ApiResponseError> {
    s.workers
        .get(id)
        .ok_or_else(|| ApiResponseError::not_found("printer_not_found"))
}
async fn printer(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let w = worker(&s, &id)?;
    Ok(Json(Envelope::ok(
        json!({"id":id,"display_name":w.config.display_name,"transport":w.config.transport,"device":w.config.device}),
    )))
}
async fn snapshot(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<crate::model::PrinterSnapshot>>, ApiResponseError> {
    let w = worker(&s, &id)?;
    Ok(Json(Envelope::ok(w.snapshot.read().unwrap().clone())))
}
async fn printer_status(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let w = worker(&s, &id)?;
    let snapshot = w.snapshot.read().unwrap();
    Ok(Json(Envelope::ok(json!({
        "printer_id": id,
        "online": snapshot.transport.protocol_up,
        "present": snapshot.transport.present,
        "open": snapshot.transport.open,
        "status": snapshot.status,
        "updated_at": snapshot.updated_at
    }))))
}
async fn capabilities(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let w = worker(&s, &id)?;
    Ok(Json(Envelope::ok(json!(
        w.snapshot.read().unwrap().capabilities
    ))))
}

async fn printer_settings(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let w = worker(&s, &id)?;
    let configured = s
        .store
        .load_settings(&id)
        .map_err(ApiResponseError::internal)?;
    let observed = w.snapshot.read().unwrap().settings.clone();
    Ok(Json(Envelope::ok(json!({
        "printer_id": id,
        "configured": configured,
        "observed": observed
    }))))
}

async fn patch_printer_settings(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(update): Json<PrinterSettings>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    update.validate().map_err(ApiResponseError::bad_request)?;
    let w = worker(&s, &id)?.clone();
    let settings = s
        .store
        .load_settings(&id)
        .map_err(ApiResponseError::internal)?
        .map(|record| record.settings.merged_with(update.clone()))
        .unwrap_or(update);
    let application = w
        .apply_settings(settings.clone())
        .await
        .map_err(ApiResponseError::unavailable)?;
    let record = PrinterSettingsRecord {
        settings,
        updated_at: application.applied_at,
        applied_at: application.applied_at,
        verification: application.verification,
        mismatches: application.mismatches,
    };
    s.store
        .save_settings(&id, &record)
        .map_err(ApiResponseError::internal)?;
    let observed = w.snapshot.read().unwrap().settings.clone();
    Ok(Json(Envelope::ok(json!({
        "printer_id": id,
        "configured": record,
        "observed": observed
    }))))
}
async fn probe(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Envelope<Value>>), ApiResponseError> {
    let w = worker(&s, &id)?.clone();
    w.probe().await.map_err(ApiResponseError::unavailable)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Envelope::ok(json!({"printer_id":id,"probe":"completed"}))),
    ))
}

async fn create_job(
    State(s): State<AppState>,
    Path(printer_id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Envelope<Job>>), ApiResponseError> {
    let w = worker(&s, &printer_id)?.clone();
    let content = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content.starts_with("application/zpl") {
        return Err(ApiResponseError::bad_request(
            "content_type_must_be_application_zpl",
        ));
    }
    let id = Uuid::new_v4();
    let part = s.store.spool_path(id);
    let final_path = s.store.payload_path(id);
    let count_header = headers
        .get("x-zpl-label-count")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let mut job = Job {
        id,
        printer_id: printer_id.clone(),
        state: JobState::Receiving,
        created_at: now(),
        updated_at: now(),
        label_count: count_header,
        label_count_source: count_header.map(|_| "header".into()),
        origin: header_string(&headers, "x-zpl-origin"),
        description: header_string(&headers, "x-zpl-description"),
        sha256: None,
        bytes: 0,
        payload_path: None,
        error: None,
    };
    s.store.save_job(&job).map_err(ApiResponseError::internal)?;
    s.store
        .register_job(id)
        .map_err(ApiResponseError::internal)?;
    let mut file = match fs::File::create(&part).await {
        Ok(file) => file,
        Err(error) => return Err(fail_receiving(&s.store, &mut job, error)),
    };
    let mut sha = Sha256::new();
    let mut bytes = 0u64;
    let mut stream = body.into_data_stream();
    loop {
        let chunk = match stream.try_next().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => return Err(fail_receiving(&s.store, &mut job, error)),
        };
        if let Err(error) = file.write_all(&chunk).await {
            return Err(fail_receiving(&s.store, &mut job, error));
        }
        sha.update(&chunk);
        bytes += chunk.len() as u64;
    }
    if let Err(error) = file.sync_all().await {
        return Err(fail_receiving(&s.store, &mut job, error));
    }
    drop(file);
    if let Err(error) = fs::rename(&part, &final_path).await {
        return Err(fail_receiving(&s.store, &mut job, error));
    }
    if let Err(error) = crate::persist::sync_parent(&final_path) {
        return Err(fail_receiving(&s.store, &mut job, error));
    }
    let (label_count, label_count_source) = if count_header.is_some() {
        (count_header, Some("header".into()))
    } else {
        (
            parse_pq(&final_path).await,
            Some("zpl_pq_best_effort".into()),
        )
    };
    job.state = JobState::Queued;
    job.updated_at = now();
    job.label_count = label_count;
    job.label_count_source = label_count_source;
    job.sha256 = Some(format!("{:x}", sha.finalize()));
    job.bytes = bytes;
    job.payload_path = Some(final_path);
    s.store.save_job(&job).map_err(ApiResponseError::internal)?;
    let event = s.store.next_event(
        "job_queued",
        Some(printer_id),
        Some(id),
        json!({ "bytes": bytes }),
    );
    s.store
        .append_event(&event)
        .map_err(ApiResponseError::internal)?;
    if let Err(e) = w.enqueue(id) {
        job.state = JobState::Failed;
        job.error = Some(e.clone());
        s.store.save_job(&job).map_err(ApiResponseError::internal)?;
        return Err(ApiResponseError::unavailable(e));
    }
    Ok((StatusCode::ACCEPTED, Json(Envelope::ok(job))))
}
fn fail_receiving(store: &Store, job: &mut Job, error: impl std::fmt::Display) -> ApiResponseError {
    job.state = JobState::Failed;
    job.updated_at = now();
    job.error = Some(error.to_string());
    let _ = store.save_job(job);
    ApiResponseError::io(error)
}
fn header_string(h: &HeaderMap, name: &str) -> Option<String> {
    h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}
async fn parse_pq(path: &std::path::Path) -> Option<u64> {
    let mut file = fs::File::open(path).await.ok()?;
    let mut chunk = [0u8; 8192];
    let mut carry = Vec::new();
    loop {
        let n = file.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        carry.extend_from_slice(&chunk[..n]);
        if let Some(at) = carry.windows(3).position(|w| w == b"^PQ") {
            let rest = &carry[at + 3..];
            let digits: Vec<u8> = rest
                .iter()
                .copied()
                .take_while(u8::is_ascii_digit)
                .collect();
            if !digits.is_empty() {
                return std::str::from_utf8(&digits).ok()?.parse().ok();
            }
        }
        if carry.len() > 32 {
            let drain = carry.len() - 32;
            carry.drain(..drain);
        }
    }
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    cursor: usize,
    limit: Option<usize>,
}
fn limit(p: &Page) -> usize {
    p.limit.unwrap_or(50).clamp(1, 200)
}
async fn jobs(
    State(s): State<AppState>,
    Query(p): Query<Page>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let items = s
        .store
        .list_jobs_page(p.cursor, limit(&p))
        .map_err(ApiResponseError::internal)?;
    Ok(Json(Envelope::ok(
        json!({"items":items,"next_cursor":p.cursor+items.len()}),
    )))
}
async fn job(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Envelope<Job>>, ApiResponseError> {
    Ok(Json(Envelope::ok(s.store.load_job(id).map_err(|_| {
        ApiResponseError::not_found("job_not_found")
    })?)))
}
async fn events(
    State(s): State<AppState>,
    Query(p): Query<Page>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    event_page(&s, p, None)
}
async fn job_events(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(p): Query<Page>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    event_page(&s, p, Some(id))
}
fn event_page(
    s: &AppState,
    p: Page,
    job: Option<Uuid>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let (items, next_cursor): (Vec<Event>, u64) = s
        .store
        .read_events(p.cursor as u64, limit(&p), job)
        .map_err(ApiResponseError::internal)?;
    Ok(Json(Envelope::ok(
        json!({"items":items,"next_cursor":next_cursor}),
    )))
}
async fn payload(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiResponseError> {
    if s.config.storage_mode == StorageMode::MetadataOnly {
        return Err(ApiResponseError::not_found("payload_not_retained"));
    }
    let job = s
        .store
        .load_job(id)
        .map_err(|_| ApiResponseError::not_found("job_not_found"))?;
    let path = job
        .payload_path
        .ok_or_else(|| ApiResponseError::not_found("payload_not_retained"))?;
    let file = fs::File::open(path)
        .await
        .map_err(|_| ApiResponseError::not_found("payload_not_found"))?;
    let stream = tokio_util::io::ReaderStream::new(file);
    Ok((
        [(header::CONTENT_TYPE, "application/zpl")],
        Body::from_stream(stream),
    )
        .into_response())
}

async fn media(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Observed<MediaState>>>, ApiResponseError> {
    worker(&s, &id)?;
    let observed = s
        .store
        .load_media(&id)
        .map_err(ApiResponseError::internal)?
        .map(|v| Observed::value(v, None, "ledger"))
        .unwrap_or_else(|| {
            let mut v = Observed::unknown();
            v.state = crate::model::ValueState::NotConfigured;
            v.reason = Some("no_media_loaded".into());
            v
        });
    Ok(Json(Envelope::ok(observed)))
}
async fn put_media(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(def): Json<MediaDefinition>,
) -> Result<Json<Envelope<MediaState>>, ApiResponseError> {
    worker(&s, &id)?;
    if let Some(old) = s
        .store
        .load_media(&id)
        .map_err(ApiResponseError::internal)?
    {
        let history = s.store.printer_dir(&id).join(format!(
            "media-{}.json",
            old.loaded_at.format("%Y%m%dT%H%M%S%.fZ")
        ));
        crate::persist::atomic_json(&history, &old).map_err(ApiResponseError::internal)?;
    }
    let state = MediaState {
        initial_labels: def.labels_available_at_load,
        remaining_labels: def.labels_available_at_load,
        consumed_labels_total: 0,
        accounting_deficit_labels: 0,
        accounting_confidence: AccountingConfidence::Estimated,
        last_accounting_event_at: None,
        ledger_sequence: 0,
        loaded_at: now(),
        media: def,
    };
    s.store
        .save_media(&id, &state)
        .map_err(ApiResponseError::internal)?;
    Ok(Json(Envelope::ok(state)))
}
async fn unload_media(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    worker(&s, &id)?;
    let Some(old) = s
        .store
        .load_media(&id)
        .map_err(ApiResponseError::internal)?
    else {
        return Err(ApiResponseError::not_found("no_media_loaded"));
    };
    let history = s.store.printer_dir(&id).join(format!(
        "media-unloaded-{}.json",
        now().format("%Y%m%dT%H%M%S%.fZ")
    ));
    crate::persist::atomic_json(&history, &old).map_err(ApiResponseError::internal)?;
    std::fs::remove_file(s.store.media_path(&id)).map_err(ApiResponseError::io)?;
    Ok(Json(Envelope::ok(json!({"unloaded":true}))))
}
async fn adjust_media(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(adj): Json<MediaAdjustment>,
) -> Result<Json<Envelope<MediaState>>, ApiResponseError> {
    worker(&s, &id)?;
    let mut m = s
        .store
        .load_media(&id)
        .map_err(ApiResponseError::internal)?
        .ok_or_else(|| ApiResponseError::not_found("no_media_loaded"))?;
    let before = m.remaining_labels;
    if adj.delta < 0 {
        let debit = adj.delta.unsigned_abs();
        m.remaining_labels = m.remaining_labels.saturating_sub(debit);
        m.consumed_labels_total = m.consumed_labels_total.saturating_add(debit);
        m.accounting_deficit_labels = m
            .accounting_deficit_labels
            .saturating_add(debit.saturating_sub(before));
    } else {
        m.remaining_labels = m.remaining_labels.saturating_add(adj.delta as u64);
    }
    m.ledger_sequence += 1;
    m.last_accounting_event_at = Some(now());
    let ev = MediaLedgerEvent {
        sequence: m.ledger_sequence,
        at: now(),
        delta: adj.delta,
        reason: adj.reason,
        source: adj.source,
        job_id: adj.job_id,
        before,
        after: m.remaining_labels,
        deficit_after: m.accounting_deficit_labels,
        hardware_counter_before: adj.hardware_counter_before,
        hardware_counter_after: adj.hardware_counter_after,
    };
    s.store
        .append_media_ledger(&id, &ev)
        .map_err(ApiResponseError::internal)?;
    s.store
        .save_media(&id, &m)
        .map_err(ApiResponseError::internal)?;
    Ok(Json(Envelope::ok(m)))
}

pub struct ApiResponseError {
    status: StatusCode,
    code: String,
    message: String,
}
impl ApiResponseError {
    fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
    fn not_found(m: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", m)
    }
    fn bad_request(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", m)
    }
    fn unavailable(m: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", m)
    }
    fn internal(e: impl std::fmt::Display) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
    }
    fn io(e: impl std::fmt::Display) -> Self {
        Self::internal(e)
    }
}
impl IntoResponse for ApiResponseError {
    fn into_response(self) -> Response {
        let body = Envelope::<Value> {
            api_version: "v1",
            generated_at: now(),
            request_id: Uuid::new_v4(),
            data: None,
            error: Some(ApiError {
                code: self.code,
                message: self.message,
            }),
        };
        (self.status, Json(body)).into_response()
    }
}
