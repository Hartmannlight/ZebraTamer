use crate::{
    config::{Config, StorageMode},
    metrics,
    model::{
        now, ApiError, Envelope, Event, Job, JobState, MediaAdjustment, MediaDefinition,
        MediaState, Observed,
    },
    persist::Store,
    protocol_v2::{self, SubmitJob},
    worker::WorkerHandle,
};
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};
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
    pub workers: Arc<RwLock<HashMap<String, WorkerHandle>>>,
    pub printer_configs: Arc<RwLock<Vec<crate::config::PrinterConfig>>>,
    pub started: chrono::DateTime<chrono::Utc>,
    pub idempotency_lock: Arc<tokio::sync::Mutex<()>>,
}

pub fn router(state: AppState) -> Router {
    let legacy = Router::new()
        .route("/metrics", get(prometheus))
        .route("/v1/agent", get(agent))
        .route("/v1/printers", get(printers))
        .route("/v1/drivers", get(drivers))
        .route("/v1/printers/{id}", get(printer))
        .route("/v1/printers/{id}/snapshot", get(snapshot))
        .route("/v1/printers/{id}/capabilities", get(capabilities))
        .route("/v1/printers/{id}/probe", post(probe))
        .route("/v1/printers/{id}/jobs", post(create_job))
        .route("/v1/jobs", get(jobs))
        .route("/v1/jobs/{id}", get(job))
        .route("/v1/jobs/{id}/events", get(job_events))
        .route("/v1/jobs/{id}/payload", get(payload))
        .route(
            "/v1/printers/{id}/media",
            get(media)
                .put(put_media)
                .patch(edit_media)
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/v1/printers/{id}/configuration",
            get(configuration)
                .post(save_configuration)
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route(
            "/v1/printers/{id}/configuration/read",
            post(read_configuration),
        )
        .route("/v1/printers/{id}/media/unload", post(unload_media))
        .route("/v1/printers/{id}/media/adjustments", post(adjust_media))
        .route("/v1/events", get(events))
        .route_layer(middleware::from_fn_with_state(
            state.config.clone(),
            legacy_authorization,
        ));
    let router = Router::new()
        .route("/", get(root))
        .route("/healthz", get(health))
        .merge(legacy)
        .route("/v2/service", get(v2_service))
        .route("/v2/printers", get(v2_printers))
        .route("/v2/printers/{id}", get(v2_printer))
        .route("/v2/printers/{id}/queue/pause", post(v2_pause_queue))
        .route("/v2/printers/{id}/queue/resume", post(v2_resume_queue))
        .route(
            "/v2/printers/{id}/jobs",
            post(v2_create_job).layer(DefaultBodyLimit::max(protocol_v2::MAX_JOB_BYTES * 2)),
        )
        .route("/v2/jobs/{id}", get(v2_job))
        .route("/v2/jobs", get(v2_jobs))
        .route("/v2/jobs/{id}/cancel", post(v2_cancel_job))
        .route("/v2/jobs/by-idempotency/{key}", get(v2_job_by_idempotency))
        .route("/v2/admin/printers", get(v2_admin_printers))
        .route("/v2/admin/usb-devices", get(v2_admin_usb_devices))
        .route("/v2/admin/printers/{id}", post(v2_admin_save_printer))
        .route("/v2/admin/printers/{id}/media", put(v2_admin_load_media))
        .route(
            "/v2/admin/printers/{id}/maintenance/{action}",
            post(v2_admin_maintenance),
        )
        .route(
            "/v2/extensions/zebra/printers/{id}",
            post(v2_admin_save_printer),
        )
        .route(
            "/v2/extensions/zebra/printers/{id}/maintenance/{action}",
            post(v2_admin_maintenance),
        )
        .layer(DefaultBodyLimit::disable())
        .layer(TraceLayer::new_for_http());
    let router = if state.config.webui_enabled {
        router
            .route("/ui", get(crate::webui::index))
            .route("/ui/", get(crate::webui::index))
            .route("/ui/app.js", get(crate::webui::javascript))
            .route("/ui/style.css", get(crate::webui::stylesheet))
    } else {
        router
    };
    router.with_state(state)
}

async fn legacy_authorization(
    State(config): State<Arc<Config>>,
    request: Request,
    next: Next,
) -> Response {
    if crate::webui::authorized(&config, request.headers(), crate::webui::Access::Admin) {
        next.run(request).await
    } else {
        ApiResponseError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A ZebraTamer admin token is required for the legacy v1 API",
        )
        .into_response()
    }
}

async fn root() -> Json<Envelope<Value>> {
    Json(Envelope::ok(
        json!({"service":"print-agent","api_version":"v1"}),
    ))
}
async fn health() -> Json<Envelope<Value>> {
    Json(Envelope::ok(json!({"status":"ok"})))
}
async fn agent(State(s): State<AppState>) -> Json<Envelope<Value>> {
    Json(Envelope::ok(
        json!({"agent_id":s.config.agent_id,"version":env!("CARGO_PKG_VERSION"),"commit":env!("ZPL_AGENT_GIT_COMMIT"),"started_at":s.started,"uptime_seconds":(now()-s.started).num_seconds().max(0),"storage_mode":s.config.storage_mode,"printers":s.workers.read().unwrap().len(),"webui_enabled":s.config.webui_enabled}),
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
    driver: String,
    device: String,
}
async fn printers(State(s): State<AppState>) -> Json<Envelope<Vec<PrinterItem>>> {
    Json(Envelope::ok(
        s.printer_configs
            .read()
            .unwrap()
            .iter()
            .map(|p| PrinterItem {
                id: p.id.clone(),
                display_name: p.display_name.clone(),
                transport: p.transport.clone(),
                driver: p.driver.clone(),
                device: p.device.display().to_string(),
            })
            .collect(),
    ))
}

async fn drivers() -> Json<Envelope<Value>> {
    Json(Envelope::ok(json!(crate::driver::DRIVERS
        .iter()
        .map(|driver| json!({
            "id": driver.id,
            "accepted_mime_types": driver.accepted_mime_types,
            "available": driver.available,
            "device_configuration": driver.device_configuration,
            "status_probe": driver.status_probe,
        }))
        .collect::<Vec<_>>())))
}
fn worker(s: &AppState, id: &str) -> Result<WorkerHandle, ApiResponseError> {
    s.workers
        .read()
        .unwrap()
        .get(id)
        .cloned()
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
    let snapshot = w.snapshot.read().unwrap().clone();
    Ok(Json(Envelope::ok(snapshot)))
}
async fn capabilities(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let w = worker(&s, &id)?;
    let capabilities = w.snapshot.read().unwrap().capabilities.clone();
    Ok(Json(Envelope::ok(json!(capabilities))))
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
    let content_type = content.split(';').next().unwrap_or("").trim();
    let driver = crate::driver::descriptor(&w.config.driver)
        .ok_or_else(|| ApiResponseError::unavailable("configured_driver_is_unknown"))?;
    if !driver.available {
        return Err(ApiResponseError::unavailable(
            "configured_driver_is_not_available",
        ));
    }
    if !driver.accepts(content) {
        return Err(ApiResponseError::bad_request(format!(
            "content type must match driver {}; accepted: {}",
            driver.id,
            driver.accepted_mime_types.join(", ")
        )));
    }
    let idempotency_key = header_string(&headers, "x-idempotency-key");
    if idempotency_key
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 255)
    {
        return Err(ApiResponseError::bad_request(
            "x-idempotency-key must contain between 1 and 255 characters",
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
        idempotency_key: None,
        sha256: None,
        bytes: 0,
        bytes_transferred: 0,
        delivery_attempts: 0,
        payload_path: None,
        error: None,
    };
    s.store.save_job(&job).map_err(ApiResponseError::internal)?;
    let mut file = match fs::File::create(&part).await {
        Ok(file) => file,
        Err(error) => return Err(fail_receiving(&s.store, &mut job, error)),
    };
    let mut sha = Sha256::new();
    sha.update(content_type.as_bytes());
    sha.update([0]);
    let mut bytes = 0u64;
    let mut stream = body.into_data_stream();
    loop {
        let chunk = match stream.try_next().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => return Err(fail_receiving(&s.store, &mut job, error)),
        };
        if bytes.saturating_add(chunk.len() as u64) > s.config.max_job_bytes as u64 {
            s.store.remove_unregistered_job(id);
            return Err(ApiResponseError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "job_too_large",
                format!("job exceeds {} bytes", s.config.max_job_bytes),
            ));
        }
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
    let prepared_label_count = if content_type == "application/zpl" {
        None
    } else {
        let source = fs::read(&part)
            .await
            .map_err(|error| fail_receiving(&s.store, &mut job, error))?;
        let prepared =
            crate::driver::prepare_zebra_payload(content_type, &source, &w.config.device_profile)
                .map_err(|error| fail_receiving(&s.store, &mut job, error))?;
        fs::write(&part, &prepared.bytes)
            .await
            .map_err(|error| fail_receiving(&s.store, &mut job, error))?;
        let converted = fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await
            .map_err(|error| fail_receiving(&s.store, &mut job, error))?;
        converted
            .sync_all()
            .await
            .map_err(|error| fail_receiving(&s.store, &mut job, error))?;
        job.bytes = prepared.bytes.len() as u64;
        prepared.label_count
    };
    if let Err(error) = fs::rename(&part, &final_path).await {
        return Err(fail_receiving(&s.store, &mut job, error));
    }
    if let Err(error) = crate::persist::sync_parent(&final_path) {
        return Err(fail_receiving(&s.store, &mut job, error));
    }
    let (label_count, label_count_source) = if prepared_label_count.is_some() {
        (prepared_label_count, Some("raster_payload".into()))
    } else if count_header.is_some() {
        (count_header, Some("header".into()))
    } else if driver.id == "zpl" {
        (
            parse_pq(&final_path).await,
            Some("zpl_pq_best_effort".into()),
        )
    } else {
        (None, None)
    };
    job.state = JobState::Queued;
    job.updated_at = now();
    job.label_count = label_count;
    job.label_count_source = label_count_source;
    job.sha256 = Some(format!("{:x}", sha.finalize()));
    if job.bytes == 0 {
        job.bytes = bytes;
    }
    job.payload_path = Some(final_path);
    job.idempotency_key = idempotency_key.clone();
    let _idempotency_guard = s.idempotency_lock.lock().await;
    if let Some(key) = idempotency_key.as_deref() {
        if let Some(existing) = s
            .store
            .find_job_by_idempotency_key(key, id)
            .map_err(ApiResponseError::internal)?
        {
            s.store.remove_unregistered_job(id);
            if existing.printer_id == printer_id && existing.sha256 == job.sha256 {
                return Ok((StatusCode::ACCEPTED, Json(Envelope::ok(existing))));
            }
            return Err(ApiResponseError::conflict(
                "idempotency_key_reused_for_different_job",
            ));
        }
    }
    if s.store
        .count_active_jobs(&printer_id)
        .map_err(ApiResponseError::internal)?
        > s.config.max_active_jobs_per_printer
    {
        s.store.remove_unregistered_job(id);
        return Err(ApiResponseError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "queue_full",
            "printer queue has reached its configured active-job limit",
        ));
    }
    s.store.save_job(&job).map_err(ApiResponseError::internal)?;
    s.store
        .register_job(id)
        .map_err(ApiResponseError::internal)?;
    let event = s.store.next_event(
        "job_queued",
        Some(printer_id),
        Some(id),
        json!({"bytes":bytes}),
    );
    s.store
        .append_event(&event)
        .map_err(ApiResponseError::internal)?;
    if let Err(e) = w.enqueue(id).await {
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

fn v2_authorize(
    s: &AppState,
    headers: &HeaderMap,
    access: crate::webui::Access,
) -> Result<(), V2Error> {
    if crate::webui::authorized(&s.config, headers, access) {
        Ok(())
    } else {
        Err(V2Error::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid ZebraTamer bearer token is required",
        ))
    }
}

fn v2_printer_value(s: &AppState, id: &str) -> Result<Value, V2Error> {
    let w = s
        .workers
        .read()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| V2Error::not_found("printer_not_found"))?;
    let snapshot = w.snapshot.read().unwrap().clone();
    let media = crate::media::state_with_revision(&s.store, id).map_err(V2Error::internal)?;
    Ok(json!({
        "id": id,
        "service_id": s.config.agent_id,
        "display_name": w.config.display_name,
        "device_family": "zebra",
        "enabled": true,
        "accepted_mime_types": [protocol_v2::ZPL_MIME, protocol_v2::RASTER_MIME],
        "capabilities": {
            "status_probe": true,
            "media": true,
            "device_configuration": true,
            "queue_control": true,
            "zebra_maintenance": true
        },
        "profile": w.config.device_profile,
        "identity": snapshot.identity,
        "status": snapshot.status,
        "jobs": snapshot.jobs,
        "observed_at": snapshot.updated_at,
        "media": media
    }))
}

async fn v2_service(State(s): State<AppState>, headers: HeaderMap) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Read)?;
    Ok(Json(json!({
        "protocol": {"name": "printhub-print-service", "major": 2, "minor": 0},
        "service_id": s.config.agent_id,
        "service_type": "zebra_tamer",
        "display_name": "ZebraTamer",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": ["catalog", "jobs", "idempotency_lookup", "media", "zebra_configuration"],
        "started_at": s.started
    })))
}

async fn v2_printers(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Read)?;
    let items = s
        .printer_configs
        .read()
        .unwrap()
        .iter()
        .filter(|printer| printer.enabled)
        .map(|printer| v2_printer_value(&s, &printer.id))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({"items": items})))
}

async fn v2_printer(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Read)?;
    Ok(Json(v2_printer_value(&s, &id)?))
}

fn v2_job_value(job: Job) -> Value {
    let state = match job.state {
        JobState::Receiving => "receiving",
        JobState::Queued => "queued",
        JobState::Writing | JobState::Verifying => "transmitting",
        JobState::TransportAccepted => "transport_accepted",
        JobState::CompletedObserved => "completed_observed",
        JobState::Held => "held",
        JobState::Cancelled => "cancelled",
        JobState::Failed => "failed",
        JobState::OutcomeUnknown => "outcome_unknown",
    };
    json!({
        "id": job.id,
        "printer_id": job.printer_id,
        "state": state,
        "created_at": job.created_at,
        "updated_at": job.updated_at,
        "label_count": job.label_count,
        "idempotency_key": job.idempotency_key,
        "request_sha256": job.sha256,
        "device_payload_bytes": job.bytes,
        "bytes_transferred": job.bytes_transferred,
        "delivery_attempts": job.delivery_attempts,
        "error": job.error
    })
}

async fn v2_create_job(
    State(s): State<AppState>,
    Path(printer_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SubmitJob>,
) -> Result<(StatusCode, Json<Value>), V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Print)?;
    let w = s
        .workers
        .read()
        .unwrap()
        .get(&printer_id)
        .cloned()
        .ok_or_else(|| V2Error::not_found("printer_not_found"))?;
    let media_revision_changed = if let Some(expected) = request.media_revision.as_deref() {
        let actual = s
            .store
            .load_media(&printer_id)
            .map_err(V2Error::internal)?
            .as_ref()
            .map(crate::media::media_revision);
        actual.as_deref() != Some(expected)
    } else {
        false
    };
    let prepared = protocol_v2::prepare_job(&printer_id, &request, &w.config.device_profile)
        .map_err(|error| V2Error::bad_request(error.to_string()))?;
    let _guard = s.idempotency_lock.lock().await;
    if let Some(existing) = s
        .store
        .find_job_by_idempotency_key_any(&request.idempotency_key)
        .map_err(V2Error::internal)?
    {
        if existing.printer_id == printer_id
            && existing.sha256.as_deref() == Some(prepared.request_hash.as_str())
        {
            return Ok((StatusCode::ACCEPTED, Json(v2_job_value(existing))));
        }
        return Err(V2Error::new(
            StatusCode::CONFLICT,
            "idempotency_conflict",
            "The idempotency key already identifies a different job",
        ));
    }
    if s.store
        .count_active_jobs(&printer_id)
        .map_err(V2Error::internal)?
        >= s.config.max_active_jobs_per_printer
    {
        return Err(V2Error::new(
            StatusCode::TOO_MANY_REQUESTS,
            "queue_full",
            "printer queue has reached its configured active-job limit",
        ));
    }

    let id = Uuid::new_v4();
    let part = s.store.spool_path(id);
    let final_path = s.store.payload_path(id);
    let mut job = Job {
        id,
        printer_id: printer_id.clone(),
        state: JobState::Receiving,
        created_at: now(),
        updated_at: now(),
        label_count: Some(prepared.label_count),
        label_count_source: Some("v2_artifacts_and_copies".into()),
        origin: Some("print-service-v2".into()),
        description: request.description,
        idempotency_key: Some(request.idempotency_key),
        sha256: Some(prepared.request_hash),
        bytes: prepared.bytes.len() as u64,
        bytes_transferred: 0,
        delivery_attempts: 0,
        payload_path: None,
        error: None,
    };
    s.store.save_job(&job).map_err(V2Error::internal)?;
    if let Err(error) = persist_v2_payload(&part, &final_path, &prepared.bytes).await {
        job.state = JobState::Failed;
        job.updated_at = now();
        job.error = Some(error.to_string());
        let _ = s.store.save_job(&job);
        return Err(V2Error::internal(error));
    }
    job.state = if media_revision_changed {
        JobState::Held
    } else {
        JobState::Queued
    };
    job.updated_at = now();
    job.payload_path = Some(final_path);
    s.store.save_job(&job).map_err(V2Error::internal)?;
    s.store.register_job(id).map_err(V2Error::internal)?;
    s.store
        .append_event(&s.store.next_event(
            if media_revision_changed {
                "job_held"
            } else {
                "job_queued"
            },
            Some(printer_id),
            Some(id),
            json!({
                "protocol":"v2",
                "bytes":job.bytes,
                "reason": if media_revision_changed { Some("media_revision_changed") } else { None }
            }),
        ))
        .map_err(V2Error::internal)?;
    if media_revision_changed {
        job.error = Some("media_revision_changed".into());
        s.store.save_job(&job).map_err(V2Error::internal)?;
        return Ok((StatusCode::ACCEPTED, Json(v2_job_value(job))));
    }
    if let Err(error) = w.enqueue(id).await {
        job.state = JobState::Failed;
        job.updated_at = now();
        job.error = Some(error.clone());
        s.store.save_job(&job).map_err(V2Error::internal)?;
        return Err(V2Error::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "queue_unavailable",
            error,
        ));
    }
    Ok((StatusCode::ACCEPTED, Json(v2_job_value(job))))
}

async fn persist_v2_payload(
    part: &std::path::Path,
    final_path: &std::path::Path,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let mut file = fs::File::create(part).await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);
    fs::rename(part, final_path).await?;
    crate::persist::sync_parent(final_path)?;
    Ok(())
}

async fn v2_job(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Read)?;
    let job = s
        .store
        .load_job(id)
        .map_err(|_| V2Error::not_found("job_not_found"))?;
    Ok(Json(v2_job_value(job)))
}

async fn v2_jobs(
    State(s): State<AppState>,
    Query(page): Query<Page>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Read)?;
    let items = s
        .store
        .list_jobs_page(page.cursor, limit(&page))
        .map_err(V2Error::internal)?;
    let count = items.len();
    Ok(Json(json!({
        "items": items.into_iter().map(v2_job_value).collect::<Vec<_>>(),
        "next_cursor": page.cursor + count
    })))
}

async fn v2_job_by_idempotency(
    State(s): State<AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Read)?;
    let job = s
        .store
        .find_job_by_idempotency_key_any(&key)
        .map_err(V2Error::internal)?
        .ok_or_else(|| V2Error::not_found("job_not_found"))?;
    Ok(Json(v2_job_value(job)))
}

async fn v2_pause_queue(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_set_queue_paused(&s, &headers, &id, true).await
}

async fn v2_resume_queue(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_set_queue_paused(&s, &headers, &id, false).await
}

async fn v2_set_queue_paused(
    s: &AppState,
    headers: &HeaderMap,
    id: &str,
    paused: bool,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(s, headers, crate::webui::Access::Admin)?;
    let worker = s
        .workers
        .read()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| V2Error::not_found("printer_not_found"))?;
    worker
        .set_queue_paused(paused)
        .await
        .map_err(|error| V2Error::new(StatusCode::CONFLICT, "queue_control_failed", error))?;
    Ok(Json(json!({"printer_id": id, "queue_paused": paused})))
}

async fn v2_cancel_job(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Print)?;
    let job = s
        .store
        .load_job(id)
        .map_err(|_| V2Error::not_found("job_not_found"))?;
    let worker = s
        .workers
        .read()
        .unwrap()
        .get(&job.printer_id)
        .cloned()
        .ok_or_else(|| V2Error::not_found("printer_not_found"))?;
    worker
        .cancel(id)
        .await
        .map_err(|error| V2Error::new(StatusCode::CONFLICT, "job_not_cancellable", error))?;
    let cancelled = s.store.load_job(id).map_err(V2Error::internal)?;
    Ok(Json(v2_job_value(cancelled)))
}

async fn v2_admin_printers(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Admin)?;
    Ok(Json(
        json!({"items": s.printer_configs.read().unwrap().clone()}),
    ))
}

async fn v2_admin_usb_devices(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Admin)?;
    let items = tokio::task::spawn_blocking(crate::transport::discover_usb_printers)
        .await
        .map_err(V2Error::internal)?
        .map_err(V2Error::internal)?;
    Ok(Json(json!({"items": items})))
}

async fn v2_admin_save_printer(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(mut printer): Json<crate::config::PrinterConfig>,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Admin)?;
    if printer.id.is_empty() {
        printer.id = id.clone();
    }
    if printer.id != id {
        return Err(V2Error::bad_request(
            "Path printer id and document printer id must match",
        ));
    }
    let mut candidate = s.printer_configs.read().unwrap().clone();
    let existing = candidate.iter().position(|item| item.id == id);
    if existing.is_some() && s.store.has_open_jobs(&id).map_err(V2Error::internal)? {
        return Err(V2Error::new(
            StatusCode::CONFLICT,
            "printer_has_open_jobs",
            "Pause or finish all open jobs before changing this printer",
        ));
    }
    if let Some(position) = existing {
        candidate[position] = printer.clone();
    } else {
        candidate.push(printer.clone());
    }
    let mut validation = (*s.config).clone();
    validation.printers = candidate.clone();
    validation
        .validate()
        .map_err(|error| V2Error::bad_request(error.to_string()))?;
    s.store
        .save_printer_configs(&candidate)
        .map_err(V2Error::internal)?;
    *s.printer_configs.write().unwrap() = candidate;
    let mut workers = s.workers.write().unwrap();
    workers.remove(&id);
    if printer.enabled {
        workers.insert(
            id.clone(),
            crate::worker::spawn(
                printer.clone(),
                s.store.clone(),
                s.config.storage_mode,
                s.config.reconnect_debounce_secs,
                s.config.printer_reconnect_loss_labels,
                s.config.hardware_counters,
            ),
        );
    }
    drop(workers);
    s.store
        .append_event(&s.store.next_event(
            if existing.is_some() {
                "printer_updated"
            } else {
                "printer_created"
            },
            Some(id),
            None,
            json!({"enabled":printer.enabled,"transport":printer.transport}),
        ))
        .map_err(V2Error::internal)?;
    Ok(Json(json!({"printer":printer})))
}

async fn v2_admin_load_media(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(definition): Json<MediaDefinition>,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Admin)?;
    crate::media::validate(&definition).map_err(|error| V2Error::bad_request(error.to_string()))?;
    let worker = s
        .workers
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| V2Error::not_found("printer_not_found"))?;
    let result = worker
        .control(move |config, store, _, _| {
            crate::media::load(store, &config.id, definition).map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| V2Error::new(StatusCode::CONFLICT, "media_load_failed", error))?;
    Ok(Json(result))
}

async fn v2_admin_maintenance(
    State(s): State<AppState>,
    Path((id, action)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, V2Error> {
    v2_authorize(&s, &headers, crate::webui::Access::Admin)?;
    let worker = s
        .workers
        .read()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| V2Error::not_found("printer_not_found"))?;
    let result = worker
        .maintenance(action)
        .await
        .map_err(|error| V2Error::new(StatusCode::CONFLICT, "maintenance_failed", error))?;
    Ok(Json(result))
}

struct V2Error {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Value,
}

impl V2Error {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: json!({}),
        }
    }
    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }
    fn internal(error: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            error.to_string(),
        )
    }
}

impl IntoResponse for V2Error {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": {
                "code": self.code,
                "message": self.message,
                "details": self.details
            }})),
        )
            .into_response()
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
fn control_error(error: String) -> ApiResponseError {
    if error.starts_with("conflict:") {
        ApiResponseError::new(StatusCode::CONFLICT, "conflict", error)
    } else {
        ApiResponseError::new(StatusCode::BAD_GATEWAY, "device_operation_failed", error)
    }
}

async fn configuration(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let w = worker(&s, &id)?;
    let observed: Option<crate::device::Observation> =
        crate::persist::read_json(&s.store.printer_dir(&id).join("configuration.json")).ok();
    let last_save: Option<Value> =
        crate::persist::read_json(&s.store.printer_dir(&id).join("configuration-attempt.json"))
            .ok();
    let media =
        crate::media::state_with_revision(&s.store, &id).map_err(ApiResponseError::internal)?;
    Ok(Json(Envelope::ok(
        json!({"device": {"observation": observed, "profile": w.config.device_profile, "last_save": last_save},
        "media": media, "webui_enabled": s.config.webui_enabled}),
    )))
}
async fn read_configuration(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    crate::webui::authorize(&s.config, &headers)?;
    Ok(Json(Envelope::ok(
        worker(&s, &id)?
            .read_device()
            .await
            .map_err(control_error)?,
    )))
}
async fn save_configuration(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<crate::device::SaveRequest>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    crate::webui::authorize(&s.config, &headers)?;
    let w = worker(&s, &id)?;
    request
        .settings
        .validate(&w.config.device_profile)
        .map_err(|e| ApiResponseError::bad_request(e.to_string()))?;
    if !request.confirm_save_all {
        return Err(ApiResponseError::bad_request(
            "Explicit save confirmation is required",
        ));
    }
    Ok(Json(Envelope::ok(
        w.save_device(request).await.map_err(control_error)?,
    )))
}
fn media_authorization(s: &AppState, headers: &HeaderMap) -> Result<(), ApiResponseError> {
    if s.config.admin_token.is_some() {
        crate::webui::authorize(&s.config, headers)?;
    }
    Ok(())
}
async fn put_media(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(def): Json<MediaDefinition>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    media_authorization(&s, &headers)?;
    crate::media::validate(&def).map_err(|e| ApiResponseError::bad_request(e.to_string()))?;
    let result = worker(&s, &id)?
        .control(move |config, store, _, _| {
            crate::media::load(store, &config.id, def).map_err(|e| e.to_string())
        })
        .await
        .map_err(control_error)?;
    Ok(Json(Envelope::ok(result)))
}
async fn edit_media(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<crate::media::EditRequest>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    media_authorization(&s, &headers)?;
    crate::media::validate(&request.media)
        .map_err(|e| ApiResponseError::bad_request(e.to_string()))?;
    let result = worker(&s, &id)?
        .control(move |config, store, _, _| {
            crate::media::edit(store, &config.id, request).map_err(|e| e.to_string())
        })
        .await
        .map_err(control_error)?;
    Ok(Json(Envelope::ok(result)))
}
async fn unload_media(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    media_authorization(&s, &headers)?;
    let result = worker(&s, &id)?
        .control(|config, store, _, _| {
            crate::media::unload(store, &config.id).map_err(|e| e.to_string())
        })
        .await
        .map_err(control_error)?;
    Ok(Json(Envelope::ok(result)))
}
async fn adjust_media(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(adj): Json<MediaAdjustment>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    media_authorization(&s, &headers)?;
    let result = worker(&s, &id)?
        .control(move |config, store, _, _| {
            crate::media::adjust(store, &config.id, adj).map_err(|e| e.to_string())
        })
        .await
        .map_err(control_error)?;
    Ok(Json(Envelope::ok(result)))
}

pub struct ApiResponseError {
    status: StatusCode,
    code: String,
    message: String,
}
impl ApiResponseError {
    pub(crate) fn new(status: StatusCode, code: &str, message: impl Into<String>) -> Self {
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
    fn conflict(m: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", m)
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
