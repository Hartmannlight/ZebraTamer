use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub storage_mode: StorageMode,
    pub mdns_enabled: bool,
    pub poll_interval_secs: u64,
    pub capability_poll_interval_secs: u64,
    pub host_boot_loss_labels: u64,
    pub printer_reconnect_loss_labels: u64,
    pub calibration_loss_labels: u64,
    pub reconnect_debounce_secs: u64,
    pub hardware_counters: HardwareCounterPolicy,
    pub printers: Vec<PrinterConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".parse().expect("static socket address"),
            data_dir: "/var/lib/zpl-agent".into(),
            storage_mode: StorageMode::Full,
            mdns_enabled: true,
            poll_interval_secs: 30,
            capability_poll_interval_secs: 3600,
            host_boot_loss_labels: 2,
            printer_reconnect_loss_labels: 2,
            calibration_loss_labels: 0,
            reconnect_debounce_secs: 5,
            hardware_counters: HardwareCounterPolicy::Prefer,
            printers: vec![],
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading configuration {}", path.display()))?;
        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("parsing configuration {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for printer in &self.printers {
            anyhow::ensure!(!printer.id.is_empty(), "printer id must not be empty");
            anyhow::ensure!(
                printer
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "printer id {:?} contains unsafe characters",
                printer.id
            );
            anyhow::ensure!(
                ids.insert(&printer.id),
                "duplicate printer id {:?}",
                printer.id
            );
            anyhow::ensure!(
                printer.transport == "char_device",
                "unsupported transport {:?}",
                printer.transport
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    Full,
    MetadataOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HardwareCounterPolicy {
    Disabled,
    Prefer,
    Require,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrinterConfig {
    pub id: String,
    pub display_name: String,
    pub device: PathBuf,
    pub transport: String,
    pub model_hint: Option<String>,
    pub first_byte_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub write_timeout_ms: u64,
}

impl Default for PrinterConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            device: PathBuf::new(),
            transport: "char_device".into(),
            model_hint: None,
            first_byte_timeout_ms: 3000,
            idle_timeout_ms: 300,
            write_timeout_ms: 30_000,
        }
    }
}

impl PrinterConfig {
    pub fn first_byte_timeout(&self) -> Duration {
        Duration::from_millis(self.first_byte_timeout_ms)
    }
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_millis(self.idle_timeout_ms)
    }
}
