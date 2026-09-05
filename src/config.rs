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
    /// Stable identity, independent of IP address and hostname. Generated once if omitted.
    pub agent_id: Option<String>,
    pub webui_enabled: bool,
    pub admin_token: Option<String>,
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
            agent_id: None,
            webui_enabled: false,
            admin_token: None,
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
        anyhow::ensure!(
            self.admin_token
                .as_ref()
                .is_none_or(|token| token.len() >= 24),
            "admin_token must have at least 24 characters"
        );
        anyhow::ensure!(
            !self.webui_enabled
                || self
                    .admin_token
                    .as_ref()
                    .is_some_and(|token| token.len() >= 24),
            "webui_enabled requires an admin_token of at least 24 characters"
        );
        if let Some(id) = &self.agent_id {
            validate_agent_id(id)?;
        }
        let mut ids = std::collections::HashSet::new();
        for printer in &self.printers {
            let profile = &printer.device_profile;
            anyhow::ensure!(
                profile.max_width_dots > 0
                    && profile.max_length_dots > 0
                    && (1..=14).contains(&profile.max_speed_ips)
                    && (0..=9999).contains(&profile.max_offset_dots)
                    && profile
                        .resolution_dpi
                        .is_none_or(|dpi| (100..=1200).contains(&dpi)),
                "Invalid device_profile limits"
            );
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
            let driver = crate::driver::descriptor(&printer.driver)
                .ok_or_else(|| anyhow::anyhow!("unsupported driver {:?}", printer.driver))?;
            anyhow::ensure!(
                driver.available,
                "driver {:?} is reserved but not implemented by this agent build",
                printer.driver
            );
        }
        Ok(())
    }

    pub fn ensure_agent_id(&mut self) -> Result<()> {
        if self.agent_id.is_some() {
            self.validate()?;
            return Ok(());
        }
        fs::create_dir_all(&self.data_dir)?;
        let path = self.data_dir.join("agent-id");
        match fs::read_to_string(&path) {
            Ok(value) => {
                let id = value.trim().to_owned();
                validate_agent_id(&id)?;
                self.agent_id = Some(id);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                use std::io::Write;
                let id = uuid::Uuid::new_v4().to_string();
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(mut file) => {
                        file.write_all(id.as_bytes())?;
                        file.sync_all()?;
                        self.agent_id = Some(id);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let id = fs::read_to_string(&path)?.trim().to_owned();
                        validate_agent_id(&id)?;
                        self.agent_id = Some(id);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

fn validate_agent_id(id: &str) -> Result<()> {
    anyhow::ensure!(
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "agent_id must contain only letters, digits, hyphens and underscores"
    );
    Ok(())
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn generated_identity_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = Config {
            data_dir: dir.path().into(),
            ..Config::default()
        };
        first.ensure_agent_id().unwrap();
        let mut restarted = Config {
            data_dir: dir.path().into(),
            ..Config::default()
        };
        restarted.ensure_agent_id().unwrap();
        assert_eq!(first.agent_id, restarted.agent_id);
        assert!(first.agent_id.is_some());
    }

    #[test]
    fn explicit_identity_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            agent_id: Some("workshop-pi".into()),
            data_dir: dir.path().into(),
            ..Config::default()
        };
        config.ensure_agent_id().unwrap();
        assert_eq!(config.agent_id.as_deref(), Some("workshop-pi"));
        assert!(!dir.path().join("agent-id").exists());
    }

    #[test]
    fn invalid_identity_is_rejected() {
        let config = Config {
            agent_id: Some("".into()),
            ..Config::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn corrupt_persisted_identity_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("agent-id"), "").unwrap();
        let mut config = Config {
            data_dir: dir.path().into(),
            ..Config::default()
        };
        assert!(config.ensure_agent_id().is_err());
        assert_eq!(fs::read_to_string(dir.path().join("agent-id")).unwrap(), "");
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
    pub driver: String,
    pub model_hint: Option<String>,
    pub device_profile: crate::device::DeviceProfile,
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
            driver: "zpl".into(),
            model_hint: None,
            device_profile: Default::default(),
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
