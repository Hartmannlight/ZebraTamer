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
    pub auto_discover: bool,
    pub auto_discover_dir: PathBuf,
    pub auto_discover_id_prefix: String,
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
            auto_discover: false,
            auto_discover_dir: "/dev/usb".into(),
            auto_discover_id_prefix: "usb".into(),
            printers: vec![],
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("reading configuration {}", path.display()))?;
        let mut config: Self = toml::from_str(&raw)
            .with_context(|| format!("parsing configuration {}", path.display()))?;
        config.add_discovered_printers()?;
        config.validate()?;
        Ok(config)
    }

    fn add_discovered_printers(&mut self) -> Result<()> {
        if !self.auto_discover {
            return Ok(());
        }
        let Ok(entries) = fs::read_dir(&self.auto_discover_dir) else {
            return Ok(());
        };
        let mut devices: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.strip_prefix("lp")
                            .is_some_and(|suffix| suffix.chars().all(|c| c.is_ascii_digit()))
                    })
            })
            .collect();
        devices.sort();
        for device in devices {
            if self
                .printers
                .iter()
                .any(|printer| same_device(&printer.device, &device))
            {
                continue;
            }
            let kernel_name = device
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("printer");
            let base_id = format!("{}-{kernel_name}", self.auto_discover_id_prefix);
            let mut id = base_id.clone();
            let mut suffix = 2;
            while self.printers.iter().any(|printer| printer.id == id) {
                id = format!("{base_id}-{suffix}");
                suffix += 1;
            }
            self.printers.push(PrinterConfig {
                id: id.clone(),
                display_name: format!("Discovered USB printer {kernel_name}"),
                device,
                ..PrinterConfig::default()
            });
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !self.auto_discover_id_prefix.is_empty()
                && self
                    .auto_discover_id_prefix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "auto_discover_id_prefix contains unsafe characters"
        );
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

fn same_device(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
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
    pub bidirectional_queries: bool,
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
            bidirectional_queries: true,
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
    pub fn write_timeout(&self) -> Duration {
        Duration::from_millis(self.write_timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_adds_every_unconfigured_lp_device() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("lp0"), []).unwrap();
        fs::write(temp.path().join("lp1"), []).unwrap();
        fs::write(temp.path().join("not-a-printer"), []).unwrap();
        let mut config = Config {
            auto_discover: true,
            auto_discover_dir: temp.path().into(),
            printers: vec![PrinterConfig {
                id: "ente".into(),
                device: temp.path().join("lp1"),
                ..PrinterConfig::default()
            }],
            ..Config::default()
        };
        config.add_discovered_printers().unwrap();
        assert_eq!(config.printers.len(), 2);
        assert!(config
            .printers
            .iter()
            .any(|printer| printer.id == "usb-lp0"));
        assert_eq!(
            config
                .printers
                .iter()
                .filter(|printer| printer.device.ends_with("lp1"))
                .count(),
            1
        );
    }

    #[test]
    fn bidirectional_queries_default_on_and_can_be_disabled() {
        let default: PrinterConfig = toml::from_str("").unwrap();
        let disabled: PrinterConfig = toml::from_str("bidirectional_queries = false").unwrap();
        assert!(default.bidirectional_queries);
        assert!(!disabled.bidirectional_queries);
    }
}
