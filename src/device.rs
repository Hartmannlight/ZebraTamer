//! Explicit, serialized device configuration. Never replay configuration on a print job.
use crate::{
    config::PrinterConfig,
    model::now,
    persist::{atomic_json, Store},
    transport::PrinterTransport,
};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceProfile {
    pub resolution_dpi: Option<u32>,
    pub max_width_dots: u32,
    pub max_length_dots: u32,
    pub max_speed_ips: u32,
    pub max_offset_dots: i32,
    pub thermal_transfer: bool,
    pub peel_off: bool,
    pub cutter: bool,
}
impl Default for DeviceProfile {
    fn default() -> Self {
        Self {
            resolution_dpi: None,
            max_width_dots: 832,
            max_length_dots: 32000,
            max_speed_ips: 6,
            max_offset_dots: 64,
            thermal_transfer: false,
            peel_off: false,
            cutter: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PrintMode {
    TearOff,
    PeelOff,
    Cutter,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PrintMethod {
    DirectThermal,
    ThermalTransfer,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Tracking {
    Gap,
    BlackMark,
    Continuous,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DeviceSettings {
    pub darkness: Option<f64>,
    pub print_speed: Option<u32>,
    pub x_offset: Option<i32>,
    pub y_offset: Option<i32>,
    pub print_width: Option<u32>,
    pub label_length: Option<u32>,
    pub print_mode: Option<PrintMode>,
    pub print_method: Option<PrintMethod>,
    pub tracking: Option<Tracking>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub settings: DeviceSettings,
    pub resolution_dpi: Option<u32>,
    pub revision: String,
    pub observed_at: chrono::DateTime<chrono::Utc>,
    pub raw: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRequest {
    pub revision: String,
    pub settings: DeviceSettings,
    pub confirm_save_all: bool,
}

pub fn revision(value: &impl Serialize) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("serializable configuration"))
    )
}

impl DeviceSettings {
    pub fn validate(&self, profile: &DeviceProfile) -> Result<()> {
        ensure!(*self != Self::default(), "No device settings selected");
        if let Some(n) = self.darkness {
            ensure!(
                n.is_finite() && (0.0..=30.0).contains(&n),
                "Darkness must be between 0 and 30"
            );
        }
        if let Some(n) = self.print_speed {
            ensure!(
                (1..=profile.max_speed_ips).contains(&n),
                "Print speed exceeds configured hardware limits"
            );
        }
        for n in [self.x_offset, self.y_offset].into_iter().flatten() {
            ensure!(
                n >= -profile.max_offset_dots && n <= profile.max_offset_dots,
                "Offset exceeds configured hardware limits"
            );
        }
        if let Some(n) = self.print_width {
            ensure!(
                (1..=profile.max_width_dots).contains(&n),
                "Print width exceeds configured hardware limits"
            );
        }
        if let Some(n) = self.label_length {
            ensure!(
                (1..=profile.max_length_dots).contains(&n),
                "Label length exceeds configured hardware limits"
            );
        }
        ensure!(
            self.print_method != Some(PrintMethod::ThermalTransfer) || profile.thermal_transfer,
            "Thermal transfer is not enabled in this printer's hardware profile"
        );
        ensure!(
            self.print_mode != Some(PrintMode::PeelOff) || profile.peel_off,
            "Peel-off is not enabled in this printer's hardware profile"
        );
        ensure!(
            self.print_mode != Some(PrintMode::Cutter) || profile.cutter,
            "Cutter is not enabled in this printer's hardware profile"
        );
        Ok(())
    }

    pub fn commands(&self) -> String {
        let mut zpl = String::from("^XA\n");
        // ~SD is absolute device darkness; ^MD is a relative format adjustment.
        if let Some(v) = self.darkness {
            zpl.push_str(&format!("~SD{v}\n"));
        }
        if let Some(v) = self.print_speed {
            zpl.push_str(&format!("^PR{v}\n"));
        }
        if let Some(v) = self.x_offset {
            zpl.push_str(&format!("^LS{v}\n"));
        }
        if let Some(v) = self.y_offset {
            zpl.push_str(&format!("^LT{v}\n"));
        }
        if let Some(v) = self.print_width {
            zpl.push_str(&format!("^PW{v}\n"));
        }
        if let Some(v) = self.label_length {
            zpl.push_str(&format!("^LL{v}\n"));
        }
        if let Some(v) = self.print_mode {
            zpl.push_str(match v {
                PrintMode::TearOff => "^MMT\n",
                PrintMode::PeelOff => "^MMP\n",
                PrintMode::Cutter => "^MMC\n",
            });
        }
        if let Some(v) = self.print_method {
            zpl.push_str(match v {
                PrintMethod::DirectThermal => "^MTD\n",
                PrintMethod::ThermalTransfer => "^MTT\n",
            });
        }
        if let Some(v) = self.tracking {
            zpl.push_str(match v {
                Tracking::Gap => "^MNY\n",
                Tracking::BlackMark => "^MNM\n",
                Tracking::Continuous => "^MNN\n",
            });
        }
        zpl.push_str("^XZ\n");
        zpl
    }

    pub fn mismatches(&self, actual: &Self) -> Vec<String> {
        let wanted = serde_json::to_value(self).unwrap();
        let actual = serde_json::to_value(actual).unwrap();
        wanted
            .as_object()
            .unwrap()
            .iter()
            .filter(|&(key, value)| !value.is_null() && actual.get(key) != Some(value))
            .map(|(key, _)| key.clone())
            .collect()
    }
}

// ^HH reports vary by firmware. Only parse recognized, unambiguous values.
// Unrecognized fields remain None and cannot be written by this API.
pub fn parse_configuration(raw: &str, profile: &DeviceProfile) -> Result<Observation> {
    let mut fields = std::collections::BTreeMap::new();
    let labels = [
        "DARKNESS",
        "PRINT SPEED",
        "LEFT POSITION",
        "LABEL TOP",
        "PRINT WIDTH",
        "LABEL LENGTH",
        "PRINT MODE",
        "PRINT METHOD",
        "MEDIA TYPE",
        "SENSOR TYPE",
        "RESOLUTION",
    ];
    for line in raw.lines() {
        let line = line.trim_matches(|c: char| c.is_control() || c.is_whitespace());
        let upper = line.to_ascii_uppercase();
        for label in labels {
            if let Some(value) = upper.strip_suffix(label) {
                fields.insert(label, value.trim().to_owned());
            }
        }
    }
    let get = |key| fields.get(key).map(String::as_str).unwrap_or("");
    let first = |key| get(key).split_whitespace().next().unwrap_or("");
    let normalized = |key| get(key).replace(['-', ' '], "_");
    let dots = |key| {
        let value = get(key);
        // Old firmware may report "050 6/8 MM". Never interpret that as 50 dots.
        if value.split_whitespace().count() == 1 {
            value.parse::<u32>().ok()
        } else {
            None
        }
    };
    let settings = DeviceSettings {
        darkness: first("DARKNESS")
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite()),
        print_speed: first("PRINT SPEED").parse().ok(),
        x_offset: first("LEFT POSITION").parse().ok(),
        y_offset: first("LABEL TOP").parse().ok(),
        print_width: dots("PRINT WIDTH"),
        label_length: dots("LABEL LENGTH"),
        print_mode: match normalized("PRINT MODE").as_str() {
            "TEAR_OFF" => Some(PrintMode::TearOff),
            "PEEL_OFF" => Some(PrintMode::PeelOff),
            "CUTTER" => Some(PrintMode::Cutter),
            _ => None,
        },
        print_method: match normalized("PRINT METHOD").as_str() {
            "DIRECT_THERMAL" => Some(PrintMethod::DirectThermal),
            "THERMAL_TRANS" | "THERMAL_TRANSFER" => Some(PrintMethod::ThermalTransfer),
            _ => None,
        },
        tracking: match normalized("MEDIA TYPE").as_str() {
            "CONTINUOUS" => Some(Tracking::Continuous),
            "GAP/NOTCH" | "WEB" => Some(Tracking::Gap),
            "MARK" | "BLACK_MARK" => Some(Tracking::BlackMark),
            "NON_CONTINUOUS" => match get("SENSOR TYPE") {
                "WEB" => Some(Tracking::Gap),
                "MARK" => Some(Tracking::BlackMark),
                _ => None,
            },
            _ => None,
        },
    };
    ensure!(
        settings != DeviceSettings::default(),
        "Printer returned no recognized configuration values"
    );
    let resolution_dpi = get("RESOLUTION")
        .split_whitespace()
        .find_map(|word| {
            word.strip_suffix("/MM")
                .and_then(|n| n.parse::<f64>().ok())
                .filter(|n| n.is_finite() && *n > 0.0)
                .map(|n| match n as u32 {
                    8 => 203,
                    12 => 300,
                    24 => 600,
                    _ => (n * 25.4).round() as u32,
                })
        })
        .or(profile.resolution_dpi);
    // Catch changes to other persistent parameters too: ^JUS saves all of them.
    // RTC values are volatile and must not make every editor stale after a second.
    let stable_lines: Vec<_> = raw
        .lines()
        .map(str::trim)
        .filter(|line| {
            !["RTC DATE", "RTC TIME", "UPTIME"]
                .iter()
                .any(|suffix| line.to_ascii_uppercase().ends_with(suffix))
        })
        .collect();
    let revision = revision(&stable_lines);
    Ok(Observation {
        settings,
        resolution_dpi,
        revision,
        observed_at: now(),
        raw: raw.into(),
    })
}

pub fn read_configuration(
    transport: &mut dyn PrinterTransport,
    config: &PrinterConfig,
) -> Result<Observation> {
    let response = transport.query(
        b"^XA^HH^XZ",
        config.first_byte_timeout(),
        config.idle_timeout(),
    )?;
    ensure!(
        response.classification != "printer_error",
        "Printer rejected configuration query"
    );
    parse_configuration(
        &String::from_utf8_lossy(&response.bytes),
        &config.device_profile,
    )
}

pub fn save_configuration(
    transport: &mut dyn PrinterTransport,
    config: &PrinterConfig,
    store: &Store,
    request: &SaveRequest,
) -> Result<Value> {
    ensure!(
        request.confirm_save_all,
        "Confirm that all current persistent device settings will be saved"
    );
    request.settings.validate(&config.device_profile)?;
    let before = read_configuration(transport, config)?;
    ensure!(
        before.revision == request.revision,
        "conflict: Device configuration changed; read it again before saving"
    );
    let old = serde_json::to_value(&before.settings)?;
    for (key, value) in serde_json::to_value(&request.settings)?
        .as_object()
        .unwrap()
    {
        ensure!(
            value.is_null() || !old[key].is_null(),
            "Cannot safely verify unsupported or unreadable field: {key}"
        );
    }
    if let Some(length) = request.settings.label_length {
        let tracking = request.settings.tracking.or(before.settings.tracking);
        ensure!(tracking == Some(Tracking::Continuous) || before.settings.label_length == Some(length),
            "For gap/mark labels the printer determines length by calibration; only continuous-media length may be changed here");
    }
    let attempt_path = store
        .printer_dir(&config.id)
        .join("configuration-attempt.json");
    let mut report = json!({"requested": request.settings, "started_at": now(), "state": "in_progress",
        "save_command_sent": false, "active_values_verified": false, "power_cycle_verified": false});
    atomic_json(&attempt_path, &report)?;
    let result = (|| -> Result<()> {
        transport.write_bytes(request.settings.commands().as_bytes())?;
        let active = read_configuration(transport, config)?;
        atomic_json(
            &store.printer_dir(&config.id).join("configuration.json"),
            &active,
        )?;
        let mismatches = request.settings.mismatches(&active.settings);
        if !mismatches.is_empty() {
            report["state"] = json!("not_saved");
            bail!("Device did not confirm fields: {}. Runtime values may have changed; nothing was saved to nonvolatile memory.", mismatches.join(", "));
        }
        report["active_values_verified"] = json!(true);
        // This is one worker operation: no print or poll can interleave with this save.
        report["state"] = json!("save_outcome_unknown");
        atomic_json(&attempt_path, &report)?;
        transport.write_bytes(b"^XA^JUS^XZ\n")?;
        report["save_command_sent"] = json!(true);
        let after = read_configuration(transport, config)?;
        atomic_json(
            &store.printer_dir(&config.id).join("configuration.json"),
            &after,
        )?;
        ensure!(
            request.settings.mismatches(&after.settings).is_empty(),
            "Readback after save differs from requested settings"
        );
        report["state"] = json!("save_sent_active_verified");
        report["observation"] = serde_json::to_value(after)?;
        Ok(())
    })();
    if let Err(error) = result {
        if report["state"] == "in_progress" {
            report["state"] = json!("apply_outcome_unknown");
        }
        report["error"] = json!(error.to_string());
    }
    report["finished_at"] = json!(now());
    atomic_json(&attempt_path, &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{DeliveryFailure, QueryResponse};
    use std::{collections::VecDeque, path::Path, time::Duration};
    const RAW: &str = "10.0 DARKNESS\n3 IPS PRINT SPEED\n+0000 LEFT POSITION\n+000 LABEL TOP\n832 PRINT WIDTH\n400 LABEL LENGTH\nTEAR OFF PRINT MODE\nDIRECT-THERMAL PRINT METHOD\nNON-CONTINUOUS MEDIA TYPE\nWEB SENSOR TYPE\n8/MM FULL RESOLUTION\n";
    struct Fake {
        replies: VecDeque<String>,
        writes: Vec<String>,
    }
    impl PrinterTransport for Fake {
        fn query(&mut self, command: &[u8], _: Duration, _: Duration) -> Result<QueryResponse> {
            assert_eq!(command, b"^XA^HH^XZ");
            let reply = self
                .replies
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("timeout"))?;
            Ok(QueryResponse {
                bytes: reply.into_bytes(),
                duration: Duration::ZERO,
                classification: "response".into(),
            })
        }
        fn write_bytes(&mut self, data: &[u8]) -> Result<()> {
            self.writes.push(String::from_utf8(data.to_vec())?);
            Ok(())
        }
        fn write_file(&mut self, _: &Path) -> std::result::Result<u64, DeliveryFailure> {
            panic!("configuration must not become a print job")
        }
    }
    #[test]
    fn parser_is_conservative_and_commands_are_typed() {
        let observation = parse_configuration(RAW, &DeviceProfile::default()).unwrap();
        assert_eq!(observation.settings.x_offset, Some(0));
        assert_eq!(observation.resolution_dpi, Some(203));
        let old = parse_configuration(
            &RAW.replace("832 PRINT WIDTH", "050 6/8 MM PRINT WIDTH"),
            &DeviceProfile::default(),
        )
        .unwrap();
        assert_eq!(old.settings.print_width, None);
        assert!(
            serde_json::from_value::<DeviceSettings>(json!({"print_mode": "tear_off^JUF"}))
                .is_err()
        );
        let settings = DeviceSettings {
            darkness: Some(12.0),
            ..Default::default()
        };
        assert!(settings.commands().contains("~SD12"));
        assert!(!settings.commands().contains("^MD"));
        assert!(!settings.commands().contains("^JUS"));
    }
    #[test]
    fn save_verifies_before_persisting_and_does_not_claim_power_cycle_verification() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        let config = PrinterConfig {
            id: "test".into(),
            ..Default::default()
        };
        let request = SaveRequest {
            revision: parse_configuration(RAW, &config.device_profile)
                .unwrap()
                .revision,
            settings: DeviceSettings {
                darkness: Some(12.0),
                ..Default::default()
            },
            confirm_save_all: true,
        };
        let changed = RAW.replace("10.0 DARKNESS", "12.0 DARKNESS");
        let mut transport = Fake {
            replies: vec![RAW.into(), changed.clone(), changed].into(),
            writes: vec![],
        };
        let result = save_configuration(&mut transport, &config, &store, &request).unwrap();
        assert_eq!(result["state"], "save_sent_active_verified");
        assert_eq!(transport.writes.len(), 2);
        assert_eq!(transport.writes[1], "^XA^JUS^XZ\n");
        assert_eq!(result["power_cycle_verified"], false);
        let mut ignored = Fake {
            replies: vec![RAW.into(), RAW.into()].into(),
            writes: vec![],
        };
        assert_eq!(
            save_configuration(&mut ignored, &config, &store, &request).unwrap()["state"],
            "not_saved"
        );
        assert_eq!(ignored.writes.len(), 1);
        let mut offline = Fake {
            replies: vec![RAW.into()].into(),
            writes: vec![],
        };
        assert_eq!(
            save_configuration(&mut offline, &config, &store, &request).unwrap()["state"],
            "apply_outcome_unknown"
        );
        assert_eq!(offline.writes.len(), 1);
        let mut stale = Fake {
            replies: vec![RAW.into()].into(),
            writes: vec![],
        };
        let mut stale_request = request.clone();
        stale_request.revision = "old".into();
        assert!(save_configuration(&mut stale, &config, &store, &stale_request).is_err());
        assert!(stale.writes.is_empty());
    }
    #[test]
    fn unsupported_hardware_and_out_of_range_values_are_rejected() {
        for settings in [
            DeviceSettings {
                print_mode: Some(PrintMode::PeelOff),
                ..Default::default()
            },
            DeviceSettings {
                darkness: Some(31.0),
                ..Default::default()
            },
            DeviceSettings {
                x_offset: Some(i32::MIN),
                ..Default::default()
            },
            DeviceSettings {
                print_method: Some(PrintMethod::ThermalTransfer),
                ..Default::default()
            },
        ] {
            assert!(settings.validate(&DeviceProfile::default()).is_err());
        }
    }
}
