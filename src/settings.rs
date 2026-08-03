use crate::model::{now, Observed, PrinterSnapshot};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PrinterSettings {
    pub darkness: Option<f64>,
    pub print_speed_ips: Option<f64>,
    pub slew_speed_ips: Option<f64>,
    pub backfeed_speed_ips: Option<f64>,
    pub x_offset_dots: Option<i64>,
    pub y_offset_dots: Option<i64>,
    pub tear_off_dots: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SettingsVerification {
    Verified,
    Mismatch,
    AppliedUnverified,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsApplication {
    pub applied_at: DateTime<Utc>,
    pub verification: SettingsVerification,
    pub mismatches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrinterSettingsRecord {
    pub settings: PrinterSettings,
    pub updated_at: DateTime<Utc>,
    pub applied_at: DateTime<Utc>,
    pub verification: SettingsVerification,
    pub mismatches: Vec<String>,
}

impl PrinterSettings {
    pub fn merged_with(&self, update: Self) -> Self {
        Self {
            darkness: update.darkness.or(self.darkness),
            print_speed_ips: update.print_speed_ips.or(self.print_speed_ips),
            slew_speed_ips: update.slew_speed_ips.or(self.slew_speed_ips),
            backfeed_speed_ips: update.backfeed_speed_ips.or(self.backfeed_speed_ips),
            x_offset_dots: update.x_offset_dots.or(self.x_offset_dots),
            y_offset_dots: update.y_offset_dots.or(self.y_offset_dots),
            tear_off_dots: update.tear_off_dots.or(self.tear_off_dots),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self == &Self::default() {
            return Err("at_least_one_setting_is_required".into());
        }
        validate_decimal("darkness", self.darkness, 0.0, 30.0)?;
        validate_decimal("print_speed_ips", self.print_speed_ips, 1.0, 14.0)?;
        validate_decimal("slew_speed_ips", self.slew_speed_ips, 1.0, 14.0)?;
        validate_decimal("backfeed_speed_ips", self.backfeed_speed_ips, 1.0, 14.0)?;
        validate_integer("x_offset_dots", self.x_offset_dots, -9999, 9999)?;
        validate_integer("y_offset_dots", self.y_offset_dots, -120, 120)?;
        validate_integer("tear_off_dots", self.tear_off_dots, -120, 120)?;
        Ok(())
    }

    /// Build a deliberately small, allowlisted Zebra configuration script.
    /// ^JUR first discards transient settings left by previous print formats;
    /// ^JUS then saves the allowlisted changes in nonvolatile memory.
    pub fn to_persistent_zpl(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut zpl = String::from("^XA^JUR^XZ^XA");
        if let Some(value) = self.darkness {
            zpl.push_str("~SD");
            zpl.push_str(&decimal(value));
        }
        if self.print_speed_ips.is_some()
            || self.slew_speed_ips.is_some()
            || self.backfeed_speed_ips.is_some()
        {
            let mut values = vec![
                self.print_speed_ips.map(decimal).unwrap_or_default(),
                self.slew_speed_ips.map(decimal).unwrap_or_default(),
                self.backfeed_speed_ips.map(decimal).unwrap_or_default(),
            ];
            while values.last().is_some_and(String::is_empty) {
                values.pop();
            }
            zpl.push_str("^PR");
            zpl.push_str(&values.join(","));
        }
        if let Some(value) = self.x_offset_dots {
            zpl.push_str(&format!("^LS{value}"));
        }
        if let Some(value) = self.y_offset_dots {
            zpl.push_str(&format!("^LT{value}"));
        }
        if let Some(value) = self.tear_off_dots {
            zpl.push_str(&format!("~TA{value}"));
        }
        zpl.push_str("^JUS^XZ");
        Ok(zpl.into_bytes())
    }

    pub fn verify(&self, snapshot: &PrinterSnapshot) -> SettingsApplication {
        let mut mismatches = Vec::new();
        compare_float(
            "darkness",
            self.darkness,
            &snapshot.settings.darkness,
            &mut mismatches,
        );
        compare_float(
            "print_speed_ips",
            self.print_speed_ips,
            &snapshot.settings.print_speed,
            &mut mismatches,
        );
        compare_float(
            "slew_speed_ips",
            self.slew_speed_ips,
            &snapshot.settings.slew_speed,
            &mut mismatches,
        );
        compare_float(
            "backfeed_speed_ips",
            self.backfeed_speed_ips,
            &snapshot.settings.backfeed_speed,
            &mut mismatches,
        );
        compare_integer(
            "x_offset_dots",
            self.x_offset_dots,
            &snapshot.settings.x_offset_dots,
            &mut mismatches,
        );
        compare_integer(
            "y_offset_dots",
            self.y_offset_dots,
            &snapshot.settings.label_top_dots,
            &mut mismatches,
        );
        compare_integer(
            "tear_off_dots",
            self.tear_off_dots,
            &snapshot.settings.tear_off_dots,
            &mut mismatches,
        );
        SettingsApplication {
            applied_at: now(),
            verification: if mismatches.is_empty() {
                SettingsVerification::Verified
            } else {
                SettingsVerification::Mismatch
            },
            mismatches,
        }
    }
}

fn validate_decimal(name: &str, value: Option<f64>, min: f64, max: f64) -> Result<(), String> {
    let Some(value) = value else { return Ok(()) };
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(format!("{name}_must_be_between_{min}_and_{max}"));
    }
    if (value * 10.0 - (value * 10.0).round()).abs() > 0.000_001 {
        return Err(format!("{name}_supports_at_most_one_decimal_place"));
    }
    Ok(())
}

fn validate_integer(name: &str, value: Option<i64>, min: i64, max: i64) -> Result<(), String> {
    if value.is_some_and(|value| !(min..=max).contains(&value)) {
        return Err(format!("{name}_must_be_between_{min}_and_{max}"));
    }
    Ok(())
}

fn decimal(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

fn compare_float(
    name: &str,
    expected: Option<f64>,
    observed: &Observed<f64>,
    mismatches: &mut Vec<String>,
) {
    let Some(expected) = expected else { return };
    match observed.value {
        Some(actual) if (actual - expected).abs() <= 0.05 => {}
        Some(actual) => mismatches.push(format!("{name}:expected={expected},actual={actual}")),
        None => mismatches.push(format!("{name}:not_reported")),
    }
}

fn compare_integer(
    name: &str,
    expected: Option<i64>,
    observed: &Observed<i64>,
    mismatches: &mut Vec<String>,
) {
    let Some(expected) = expected else { return };
    match observed.value {
        Some(actual) if actual == expected => {}
        Some(actual) => mismatches.push(format!("{name}:expected={expected},actual={actual}")),
        None => mismatches.push(format!("{name}:not_reported")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_zpl_contains_only_configured_allowlisted_values() {
        let settings = PrinterSettings {
            darkness: Some(18.5),
            print_speed_ips: Some(4.0),
            x_offset_dots: Some(-3),
            y_offset_dots: Some(8),
            tear_off_dots: Some(12),
            ..PrinterSettings::default()
        };
        assert_eq!(
            settings.to_persistent_zpl().unwrap(),
            b"^XA^JUR^XZ^XA~SD18.5^PR4^LS-3^LT8~TA12^JUS^XZ"
        );
    }

    #[test]
    fn speed_slots_are_preserved_when_only_backfeed_is_set() {
        let settings = PrinterSettings {
            backfeed_speed_ips: Some(2.0),
            ..PrinterSettings::default()
        };
        assert_eq!(
            settings.to_persistent_zpl().unwrap(),
            b"^XA^JUR^XZ^XA^PR,,2^JUS^XZ"
        );
    }

    #[test]
    fn rejects_empty_out_of_range_and_overprecise_settings() {
        assert!(PrinterSettings::default().validate().is_err());
        assert!(PrinterSettings {
            darkness: Some(31.0),
            ..PrinterSettings::default()
        }
        .validate()
        .is_err());
        assert!(PrinterSettings {
            print_speed_ips: Some(2.25),
            ..PrinterSettings::default()
        }
        .validate()
        .is_err());
        assert!(PrinterSettings {
            y_offset_dots: Some(121),
            ..PrinterSettings::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn partial_update_keeps_existing_managed_values() {
        let existing = PrinterSettings {
            darkness: Some(18.0),
            x_offset_dots: Some(-3),
            ..PrinterSettings::default()
        };
        let merged = existing.merged_with(PrinterSettings {
            darkness: Some(20.0),
            ..PrinterSettings::default()
        });
        assert_eq!(merged.darkness, Some(20.0));
        assert_eq!(merged.x_offset_dots, Some(-3));
    }
}
