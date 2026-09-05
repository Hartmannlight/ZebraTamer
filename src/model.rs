use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

pub fn now() -> DateTime<Utc> {
    Utc::now()
}

#[derive(Debug, Clone, Serialize)]
pub struct Envelope<T: Serialize> {
    pub api_version: &'static str,
    pub generated_at: DateTime<Utc>,
    pub request_id: Uuid,
    pub data: Option<T>,
    pub error: Option<ApiError>,
}

impl<T: Serialize> Envelope<T> {
    pub fn ok(data: T) -> Self {
        Self {
            api_version: "v1",
            generated_at: now(),
            request_id: Uuid::new_v4(),
            data: Some(data),
            error: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValueState {
    Value,
    Stale,
    NotSupported,
    Unavailable,
    Unknown,
    NotConfigured,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observed<T> {
    pub state: ValueState,
    pub value: Option<T>,
    pub unit: Option<String>,
    pub source: String,
    pub observed_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

impl<T> Observed<T> {
    pub fn unknown() -> Self {
        Self {
            state: ValueState::Unknown,
            value: None,
            unit: None,
            source: "agent".into(),
            observed_at: None,
            reason: Some("not_observed_yet".into()),
        }
    }
    pub fn not_supported(reason: &str) -> Self {
        Self {
            state: ValueState::NotSupported,
            value: None,
            unit: None,
            source: "printer".into(),
            observed_at: Some(now()),
            reason: Some(reason.into()),
        }
    }
    pub fn value(value: T, unit: Option<&str>, source: &str) -> Self {
        Self {
            state: ValueState::Value,
            value: Some(value),
            unit: unit.map(str::to_owned),
            source: source.into(),
            observed_at: Some(now()),
            reason: None,
        }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            state: ValueState::Unavailable,
            value: None,
            unit: None,
            source: "printer".into(),
            observed_at: Some(now()),
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrinterSnapshot {
    pub schema_version: u32,
    pub printer_id: String,
    pub transport: TransportSnapshot,
    pub identity: IdentitySnapshot,
    pub status: StatusSnapshot,
    pub settings: SettingsSnapshot,
    pub diagnostics: DiagnosticsSnapshot,
    pub memory: MemorySnapshot,
    pub counters: CounterSnapshot,
    pub media: Observed<MediaState>,
    pub jobs: JobSummary,
    pub capabilities: BTreeMap<String, Observed<bool>>,
    pub raw_responses: BTreeMap<String, RawResponse>,
    pub query_stats: BTreeMap<String, QueryStats>,
    pub updated_at: DateTime<Utc>,
}

macro_rules! obs_struct {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Debug, Clone, Serialize, Deserialize)] pub struct $name { $(pub $field: Observed<$ty>,)* }
        impl Default for $name { fn default() -> Self { Self { $($field: Observed::unknown(),)* } } }
    };
}

obs_struct!(TransportSnapshot { present: bool, open: bool, protocol_up: bool, connected_at: DateTime<Utc>, disconnected_at: DateTime<Utc>, last_response_at: DateTime<Utc> });
obs_struct!(IdentitySnapshot {
    model: String,
    firmware: String,
    hardware_id: String,
    serial_number: String,
    resolution_dpi: u64
});
obs_struct!(StatusSnapshot {
    ready: bool,
    paused: bool,
    media_out: bool,
    ribbon_out: bool,
    head_open: bool,
    temperature_fault: bool,
    buffer_available_bytes: u64,
    print_mode: String,
    batch_remaining: u64
});
obs_struct!(SettingsSnapshot {
    darkness: f64,
    print_speed: f64,
    slew_speed: f64,
    backfeed_speed: f64,
    label_length_dots: u64,
    print_width_dots: u64,
    label_top_dots: i64,
    x_offset_dots: i64,
    y_offset_dots: i64,
    format_prefix: String,
    control_prefix: String,
    delimiter: String,
    communication: String
});
obs_struct!(DiagnosticsSnapshot {
    head_test: String,
    temperature_celsius: f64,
    sensor_values: Value,
    voltage_raw: Value,
    pitch_values: Value
});
obs_struct!(MemorySnapshot {
    ram_free_bytes: u64,
    ram_total_bytes: u64,
    flash_free_bytes: u64,
    flash_total_bytes: u64,
    other: Value
});
obs_struct!(CounterSnapshot {
    odometer: u64,
    labels_printed: u64,
    marker_count: u64,
    manual_feed_detection: bool
});

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobSummary {
    pub queue_depth: u64,
    pub open_jobs: u64,
    pub last_job_id: Option<Uuid>,
    pub last_job_state: Option<JobState>,
}

impl PrinterSnapshot {
    pub fn new(id: String) -> Self {
        Self {
            schema_version: 1,
            printer_id: id,
            transport: Default::default(),
            identity: Default::default(),
            status: Default::default(),
            settings: Default::default(),
            diagnostics: Default::default(),
            memory: Default::default(),
            counters: Default::default(),
            media: Observed::unknown(),
            jobs: Default::default(),
            capabilities: BTreeMap::new(),
            raw_responses: BTreeMap::new(),
            query_stats: BTreeMap::new(),
            updated_at: now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawResponse {
    pub text: String,
    pub base64: String,
    pub observed_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub error: Option<String>,
    pub classification: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QueryStats {
    pub count: u64,
    pub duration_ms_total: u64,
    pub response_bytes_total: u64,
    pub timeouts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Receiving,
    Queued,
    Writing,
    Verifying,
    TransportAccepted,
    CompletedObserved,
    Failed,
    OutcomeUnknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub printer_id: String,
    pub state: JobState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub label_count: Option<u64>,
    pub label_count_source: Option<String>,
    pub origin: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    pub sha256: Option<String>,
    pub bytes: u64,
    pub payload_path: Option<PathBuf>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub sequence: u64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub printer_id: Option<String>,
    pub job_id: Option<Uuid>,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingConfidence {
    Exact,
    Estimated,
    Degraded,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaState {
    pub media: MediaDefinition,
    pub loaded_at: DateTime<Utc>,
    pub initial_labels: u64,
    pub remaining_labels: u64,
    pub consumed_labels_total: u64,
    pub accounting_deficit_labels: u64,
    pub accounting_confidence: AccountingConfidence,
    pub last_accounting_event_at: Option<DateTime<Utc>>,
    pub ledger_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaDefinition {
    pub display_name: String,
    pub product_number: Option<String>,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
    pub width_mm: f64,
    pub height_mm: f64,
    pub gap_mm: Option<f64>,
    pub corner_radius_mm: Option<f64>,
    pub black_mark_width_mm: Option<f64>,
    pub black_mark_height_mm: Option<f64>,
    pub core_diameter_mm: Option<f64>,
    pub outer_diameter_mm: Option<f64>,
    pub shape: String,
    pub tracking: TrackingType,
    pub print_technology: PrintTechnology,
    pub color: MediaColor,
    pub material: Option<String>,
    pub surface: Option<String>,
    pub transparent: Option<bool>,
    pub thermal_coating: Option<String>,
    pub thickness_micrometers: Option<f64>,
    pub adhesive: Option<String>,
    pub liner: Option<String>,
    pub ribbon: Option<Ribbon>,
    pub preferred_settings: PreferredSettings,
    pub labels_available_at_load: u64,
    pub nominal_full_roll_labels: Option<u64>,
    pub low_warning_threshold: u64,
    #[serde(default)]
    pub custom: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackingType {
    Gap,
    BlackMark,
    Continuous,
    Notch,
    Hole,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrintTechnology {
    DirectThermal,
    ThermalTransfer,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaColor {
    pub name: String,
    pub hex: Option<String>,
    pub description: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ribbon {
    pub name: String,
    pub width_mm: Option<f64>,
    pub material: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PreferredSettings {
    pub darkness: Option<f64>,
    pub speed: Option<f64>,
    pub x_offset: Option<i64>,
    pub y_offset: Option<i64>,
    pub print_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaAdjustment {
    pub delta: i64,
    pub reason: String,
    pub source: String,
    pub job_id: Option<Uuid>,
    pub hardware_counter_before: Option<u64>,
    pub hardware_counter_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaLedgerEvent {
    pub sequence: u64,
    pub at: DateTime<Utc>,
    pub delta: i64,
    pub reason: String,
    pub source: String,
    pub job_id: Option<Uuid>,
    pub before: u64,
    pub after: u64,
    pub deficit_after: u64,
    pub hardware_counter_before: Option<u64>,
    pub hardware_counter_after: Option<u64>,
}
