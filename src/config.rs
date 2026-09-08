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
    /// Bearer token for read-only API clients. The admin token also grants this scope.
    pub read_token: Option<String>,
    pub read_token_file: Option<PathBuf>,
    /// Bearer token for print clients. The admin token also grants this scope.
    pub print_token: Option<String>,
    pub print_token_file: Option<PathBuf>,
    pub admin_token: Option<String>,
    pub admin_token_file: Option<PathBuf>,
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
    /// Maximum accepted device payload after decoding/conversion.
    pub max_job_bytes: usize,
    /// Backpressure limit per printer; terminal history is not counted.
    pub max_active_jobs_per_printer: usize,
    pub hardware_counters: HardwareCounterPolicy,
    pub printers: Vec<PrinterConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            agent_id: None,
            webui_enabled: false,
            read_token: None,
            read_token_file: None,
            print_token: None,
            print_token_file: None,
            admin_token: None,
            admin_token_file: None,
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
            max_job_bytes: crate::protocol_v2::MAX_JOB_BYTES,
            max_active_jobs_per_printer: 1000,
            hardware_counters: HardwareCounterPolicy::Prefer,
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
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        config.read_token = resolve_token(
            "read_token",
            config.read_token.take(),
            config.read_token_file.as_deref(),
            base,
        )?;
        config.print_token = resolve_token(
            "print_token",
            config.print_token.take(),
            config.print_token_file.as_deref(),
            base,
        )?;
        config.admin_token = resolve_token(
            "admin_token",
            config.admin_token.take(),
            config.admin_token_file.as_deref(),
            base,
        )?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        for (name, token) in [
            ("read_token", &self.read_token),
            ("print_token", &self.print_token),
            ("admin_token", &self.admin_token),
        ] {
            anyhow::ensure!(
                token.as_ref().is_none_or(|token| token.len() >= 24),
                "{name} must have at least 24 characters"
            );
        }
        anyhow::ensure!(
            !self.webui_enabled
                || self
                    .admin_token
                    .as_ref()
                    .is_some_and(|token| token.len() >= 24),
            "webui_enabled requires an admin_token of at least 24 characters"
        );
        anyhow::ensure!(
            self.max_job_bytes > 0 && self.max_job_bytes <= crate::protocol_v2::MAX_JOB_BYTES,
            "max_job_bytes must be between 1 and {}",
            crate::protocol_v2::MAX_JOB_BYTES
        );
        anyhow::ensure!(
            self.max_active_jobs_per_printer > 0,
            "max_active_jobs_per_printer must be positive"
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
            match printer.transport.as_str() {
                "char_device" => anyhow::ensure!(
                    !printer.device.as_os_str().is_empty(),
                    "char_device transport requires device"
                ),
                "usb_bulk" => anyhow::ensure!(
                    printer.usb_vendor_id.is_some() && printer.usb_product_id.is_some(),
                    "usb_bulk transport requires usb_vendor_id and usb_product_id"
                ),
                "tcp" => {
                    anyhow::ensure!(
                        printer
                            .tcp_host
                            .as_deref()
                            .is_some_and(|host| !host.trim().is_empty()),
                        "tcp transport requires tcp_host"
                    );
                    anyhow::ensure!(printer.tcp_port > 0, "tcp_port must be positive");
                    anyhow::ensure!(
                        printer.connect_timeout_ms > 0,
                        "connect_timeout_ms must be positive"
                    );
                }
                _ => anyhow::bail!("unsupported transport {:?}", printer.transport),
            }
            anyhow::ensure!(
                printer.first_byte_timeout_ms > 0
                    && printer.idle_timeout_ms > 0
                    && printer.write_timeout_ms > 0
                    && printer.max_response_bytes > 0,
                "printer timeouts and max_response_bytes must be positive"
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

    #[test]
    fn token_files_are_relative_to_the_configuration_and_reject_ambiguity() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("admin-token"),
            "admin-token-from-file-123456789\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("agent.toml"),
            "webui_enabled = true\nadmin_token_file = \"admin-token\"\n",
        )
        .unwrap();
        let loaded = Config::load(&dir.path().join("agent.toml")).unwrap();
        assert_eq!(
            loaded.admin_token.as_deref(),
            Some("admin-token-from-file-123456789")
        );

        fs::write(
            dir.path().join("ambiguous.toml"),
            "admin_token = \"inline-admin-token-123456789\"\nadmin_token_file = \"admin-token\"\n",
        )
        .unwrap();
        assert!(Config::load(&dir.path().join("ambiguous.toml")).is_err());
    }
}

fn resolve_token(
    name: &str,
    inline: Option<String>,
    file: Option<&Path>,
    config_dir: &Path,
) -> Result<Option<String>> {
    anyhow::ensure!(
        inline.is_none() || file.is_none(),
        "configure only one of {name} and {name}_file"
    );
    let Some(path) = file else {
        return Ok(inline);
    };
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_dir.join(path)
    };
    let token = fs::read_to_string(&path)
        .with_context(|| format!("reading {name} from {}", path.display()))?
        .trim()
        .to_owned();
    Ok(Some(token))
}

#[cfg(test)]
mod transport_config_tests {
    use super::*;

    #[test]
    fn usb_bulk_requires_an_explicit_device_identity() {
        let mut config = Config::default();
        config.printers.push(PrinterConfig {
            id: "usb-zebra".into(),
            transport: "usb_bulk".into(),
            driver: "zpl".into(),
            ..PrinterConfig::default()
        });
        assert!(config.validate().is_err());

        config.printers[0].usb_vendor_id = Some(0x0a5f);
        config.printers[0].usb_product_id = Some(0x00a3);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn char_device_still_requires_a_path() {
        let mut config = Config::default();
        config.printers.push(PrinterConfig {
            id: "local-zebra".into(),
            driver: "zpl".into(),
            ..PrinterConfig::default()
        });
        assert!(config.validate().is_err());
        config.printers[0].device = "/dev/usb/lp0".into();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn tcp_requires_a_host_and_uses_jetdirect_default_port() {
        let mut config = Config::default();
        config.printers.push(PrinterConfig {
            id: "network-zebra".into(),
            transport: "tcp".into(),
            driver: "zpl".into(),
            ..PrinterConfig::default()
        });
        assert!(config.validate().is_err());
        config.printers[0].tcp_host = Some("192.0.2.10".into());
        assert_eq!(config.printers[0].tcp_port, 9100);
        assert!(config.validate().is_ok());
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
    pub enabled: bool,
    pub device: PathBuf,
    pub transport: String,
    pub usb_vendor_id: Option<u16>,
    pub usb_product_id: Option<u16>,
    pub usb_serial: Option<String>,
    pub tcp_host: Option<String>,
    pub tcp_port: u16,
    pub connect_timeout_ms: u64,
    pub driver: String,
    pub model_hint: Option<String>,
    pub device_profile: crate::device::DeviceProfile,
    pub first_byte_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub write_timeout_ms: u64,
    pub max_response_bytes: usize,
}

impl Default for PrinterConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            enabled: true,
            device: PathBuf::new(),
            transport: "char_device".into(),
            usb_vendor_id: None,
            usb_product_id: None,
            usb_serial: None,
            tcp_host: None,
            tcp_port: 9100,
            connect_timeout_ms: 3000,
            driver: "zpl".into(),
            model_hint: None,
            device_profile: Default::default(),
            first_byte_timeout_ms: 3000,
            idle_timeout_ms: 300,
            write_timeout_ms: 30_000,
            max_response_bytes: 1024 * 1024,
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
