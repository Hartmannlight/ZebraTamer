use crate::{
    config::{Config, StorageMode},
    metrics,
    model::{
        now, ApiError, Envelope, Event, Job, JobState, MediaAdjustment, MediaDefinition,
        MediaState, Observed,
    },
    persist::Store,
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
    let router = Router::new()
        .route("/", get(root))
        .route("/healthz", get(health))
        .route("/metrics", get(prometheus))
        .route("/v1/agent", get(agent))
        .route("/v1/printers", get(printers))
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
        json!({"agent_id":s.config.agent_id,"version":env!("CARGO_PKG_VERSION"),"commit":env!("ZPL_AGENT_GIT_COMMIT"),"started_at":s.started,"uptime_seconds":(now()-s.started).num_seconds().max(0),"storage_mode":s.config.storage_mode,"printers":s.workers.len(),"webui_enabled":s.config.webui_enabled}),
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
}
async fn printers(State(s): State<AppState>) -> Json<Envelope<Vec<PrinterItem>>> {
    Json(Envelope::ok(
        s.config
            .printers
            .iter()
            .map(|p| PrinterItem {
                id: p.id.clone(),
                display_name: p.display_name.clone(),
                transport: p.transport.clone(),
                device: p.device.display().to_string(),
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
async fn capabilities(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Value>>, ApiResponseError> {
    let w = worker(&s, &id)?;
    Ok(Json(Envelope::ok(json!(
        w.snapshot.read().unwrap().capabilities
    ))))
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
        json!({"bytes":bytes}),
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
