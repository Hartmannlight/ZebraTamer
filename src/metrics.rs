use crate::{api::AppState, model::AccountingConfidence};
use std::fmt::Write;

pub fn render(state: &AppState) -> String {
    let mut out = String::with_capacity(16 * 1024);
    let uptime = (crate::model::now() - state.started).num_seconds().max(0);
    out.push_str(
        "# HELP zpl_agent_build_info Build information.\n# TYPE zpl_agent_build_info gauge\n",
    );
    let _ = writeln!(
        out,
        "zpl_agent_build_info{{version=\"{}\",commit=\"{}\"}} 1",
        env!("CARGO_PKG_VERSION"),
        env!("ZPL_AGENT_GIT_COMMIT")
    );
    out.push_str("# TYPE zpl_agent_uptime_seconds gauge\n");
    let _ = writeln!(out, "zpl_agent_uptime_seconds {uptime}");
    let free = filesystem_free(state.store.root());
    out.push_str("# TYPE zpl_agent_filesystem_free_bytes gauge\n");
    let _ = writeln!(out, "zpl_agent_filesystem_free_bytes {free}");
    for (id, w) in state.workers.iter() {
        let s = w.snapshot.read().unwrap();
        let p = esc(id);
        observed_bool(
            &mut out,
            "zpl_printer_present",
            &p,
            "transport_present",
            &s.transport.present,
        );
        observed_bool(
            &mut out,
            "zpl_printer_open",
            &p,
            "transport_open",
            &s.transport.open,
        );
        observed_bool(
            &mut out,
            "zpl_printer_protocol_up",
            &p,
            "protocol_up",
            &s.transport.protocol_up,
        );
        observed_bool(&mut out, "zpl_printer_ready", &p, "ready", &s.status.ready);
        metric(
            &mut out,
            "zpl_printer_job_queue_depth",
            &p,
            s.jobs.queue_depth as f64,
        );
        for fault in [
            "paused",
            "media_out",
            "ribbon_out",
            "head_open",
            "temperature",
        ] {
            let observation = match fault {
                "paused" => &s.status.paused,
                "media_out" => &s.status.media_out,
                "ribbon_out" => &s.status.ribbon_out,
                "head_open" => &s.status.head_open,
                _ => &s.status.temperature_fault,
            };
            observation_state(&mut out, &p, fault, observation);
            if let Some(value) = known_bool(observation) {
                let _ = writeln!(
                    out,
                    "zpl_printer_fault{{printer=\"{p}\",fault=\"{fault}\"}} {}",
                    if value { 1 } else { 0 }
                );
            }
        }
        for (name, observation) in [
            ("buffer_full", &s.status.buffer_full),
            ("cutter_jam", &s.status.cutter_jam),
            ("cover_open", &s.status.cover_open),
            ("clean_head_warning", &s.status.clean_head_warning),
            ("media_low", &s.status.media_low),
            ("ribbon_low", &s.status.ribbon_low),
        ] {
            observation_state(&mut out, &p, name, observation);
            if let Some(value) = known_bool(observation) {
                let _ = writeln!(
                    out,
                    "zpl_printer_fault{{printer=\"{p}\",fault=\"{name}\"}} {}",
                    if value { 1 } else { 0 }
                );
            }
        }
        let model = esc(s.identity.model.value.as_deref().unwrap_or("unknown"));
        let firmware = esc(s.identity.firmware.value.as_deref().unwrap_or("unknown"));
        let serial = esc(s
            .identity
            .serial_number
            .value
            .as_deref()
            .unwrap_or("unknown"));
        let resolution = s.identity.resolution_dpi.value.unwrap_or(0);
        let _ = writeln!(
            out,
            "zpl_printer_info{{printer=\"{p}\",model=\"{model}\",firmware=\"{firmware}\",serial_number=\"{serial}\",resolution_dpi=\"{resolution}\"}} 1"
        );
        numeric_metric(
            &mut out,
            "zpl_printer_head_temperature_celsius",
            &p,
            &s.diagnostics.temperature_celsius,
        );
        numeric_metric(&mut out, "zpl_printer_darkness", &p, &s.settings.darkness);
        numeric_metric(
            &mut out,
            "zpl_printer_print_speed_ips",
            &p,
            &s.settings.print_speed,
        );
        integer_metric(
            &mut out,
            "zpl_printer_label_length_dots",
            &p,
            &s.settings.label_length_dots,
        );
        integer_metric(
            &mut out,
            "zpl_printer_print_width_dots",
            &p,
            &s.settings.print_width_dots,
        );
        integer_metric(
            &mut out,
            "zpl_printer_memory_total_bytes",
            &p,
            &s.memory.ram_total_bytes,
        );
        integer_metric(
            &mut out,
            "zpl_printer_memory_free_bytes",
            &p,
            &s.memory.ram_free_bytes,
        );
        integer_metric(
            &mut out,
            "zpl_printer_flash_total_bytes",
            &p,
            &s.memory.flash_total_bytes,
        );
        integer_metric(
            &mut out,
            "zpl_printer_flash_free_bytes",
            &p,
            &s.memory.flash_free_bytes,
        );
        numeric_metric(
            &mut out,
            "zpl_printer_odometer_meters",
            &p,
            &s.counters.odometer_meters,
        );
        integer_metric(
            &mut out,
            "zpl_printer_labels_printed",
            &p,
            &s.counters.labels_printed,
        );
        integer_metric(
            &mut out,
            "zpl_printer_batch_remaining",
            &p,
            &s.status.batch_remaining,
        );
        integer_metric(
            &mut out,
            "zpl_printer_formats_buffered",
            &p,
            &s.status.formats_buffered,
        );
        integer_metric(
            &mut out,
            "zpl_printer_images_stored",
            &p,
            &s.status.images_stored,
        );
        numeric_metric(
            &mut out,
            "zpl_printer_head_usage_meters",
            &p,
            &s.maintenance.head_usage_meters,
        );
        numeric_metric(
            &mut out,
            "zpl_printer_last_cleaned_meters",
            &p,
            &s.maintenance.last_cleaned_meters,
        );
        integer_metric(
            &mut out,
            "zpl_printer_media_replaced",
            &p,
            &s.maintenance.media_replaced,
        );
        integer_metric(
            &mut out,
            "zpl_printer_ribbon_replaced",
            &p,
            &s.maintenance.ribbon_replaced,
        );
        integer_metric(
            &mut out,
            "zpl_printer_head_cleaned",
            &p,
            &s.maintenance.head_cleaned,
        );
        for (cap, v) in &s.capabilities {
            for state_name in [
                "value",
                "stale",
                "not_supported",
                "unavailable",
                "unknown",
                "not_configured",
            ] {
                let active = if format!("{:?}", v.state)
                    .to_ascii_lowercase()
                    .replace("notsupported", "not_supported")
                    .replace("notconfigured", "not_configured")
                    == state_name
                {
                    1
                } else {
                    0
                };
                let _=writeln!(out,"zpl_printer_capability_state{{printer=\"{p}\",capability=\"{}\",state=\"{state_name}\"}} {active}",esc(cap));
            }
        }
        for (query, stats) in &s.query_stats {
            let query = esc(query);
            let _ = writeln!(
                out,
                "zpl_printer_queries_total{{printer=\"{p}\",query=\"{query}\"}} {}",
                stats.count
            );
            let _ = writeln!(
                out,
                "zpl_printer_query_duration_seconds_total{{printer=\"{p}\",query=\"{query}\"}} {}",
                stats.duration_ms_total as f64 / 1000.0
            );
            let _ = writeln!(
                out,
                "zpl_printer_query_response_bytes_total{{printer=\"{p}\",query=\"{query}\"}} {}",
                stats.response_bytes_total
            );
            let _ = writeln!(
                out,
                "zpl_printer_query_timeouts_total{{printer=\"{p}\",query=\"{query}\"}} {}",
                stats.timeouts
            );
            let _ = writeln!(
                out,
                "zpl_printer_query_parse_errors_total{{printer=\"{p}\",query=\"{query}\"}} {}",
                stats.parse_errors
            );
        }
        if let Ok(Some(m)) = state.store.load_media(id) {
            let _ = writeln!(
                out,
                "zpl_printer_media_remaining_labels{{printer=\"{p}\"}} {}",
                m.remaining_labels
            );
            let _ = writeln!(
                out,
                "zpl_printer_media_low{{printer=\"{p}\"}} {}",
                if m.remaining_labels <= m.media.low_warning_threshold {
                    1
                } else {
                    0
                }
            );
            let confidence = match m.accounting_confidence {
                AccountingConfidence::Exact => 3,
                AccountingConfidence::Estimated => 2,
                AccountingConfidence::Degraded => 1,
                AccountingConfidence::Unknown => 0,
            };
            let _ = writeln!(
                out,
                "zpl_printer_media_accounting_confidence{{printer=\"{p}\"}} {confidence}"
            );
            let _ = writeln!(
                out,
                "zpl_printer_media_consumed_labels_total{{printer=\"{p}\",reason=\"all\"}} {}",
                m.consumed_labels_total
            );
            let _ = writeln!(
                out,
                "zpl_printer_media_accounting_deficit_labels{{printer=\"{p}\"}} {}",
                m.accounting_deficit_labels
            );
        }
    }
    out
}
fn metric(out: &mut String, name: &str, p: &str, value: f64) {
    let _ = writeln!(out, "{name}{{printer=\"{p}\"}} {value}");
}
fn known_bool(v: &crate::model::Observed<bool>) -> Option<bool> {
    (v.state == crate::model::ValueState::Value)
        .then_some(v.value)
        .flatten()
}
fn state_name(state: &crate::model::ValueState) -> &'static str {
    match state {
        crate::model::ValueState::Value => "value",
        crate::model::ValueState::Stale => "stale",
        crate::model::ValueState::NotSupported => "not_supported",
        crate::model::ValueState::Unavailable => "unavailable",
        crate::model::ValueState::Unknown => "unknown",
        crate::model::ValueState::NotConfigured => "not_configured",
    }
}
fn observation_state<T>(
    out: &mut String,
    printer: &str,
    field: &str,
    observation: &crate::model::Observed<T>,
) {
    let current = state_name(&observation.state);
    for state in [
        "value",
        "stale",
        "not_supported",
        "unavailable",
        "unknown",
        "not_configured",
    ] {
        let _ = writeln!(
            out,
            "zpl_printer_observation_state{{printer=\"{printer}\",field=\"{}\",state=\"{state}\"}} {}",
            esc(field),
            if current == state { 1 } else { 0 }
        );
    }
}
fn observed_bool(
    out: &mut String,
    name: &str,
    printer: &str,
    field: &str,
    observation: &crate::model::Observed<bool>,
) {
    observation_state(out, printer, field, observation);
    if let Some(value) = known_bool(observation) {
        metric(out, name, printer, if value { 1.0 } else { 0.0 });
    }
}
fn numeric_metric(
    out: &mut String,
    name: &str,
    printer: &str,
    observation: &crate::model::Observed<f64>,
) {
    observation_state(out, printer, name, observation);
    if let Some(value) = observation
        .value
        .filter(|_| observation.state == crate::model::ValueState::Value)
    {
        metric(out, name, printer, value);
    }
}
fn integer_metric(
    out: &mut String,
    name: &str,
    printer: &str,
    observation: &crate::model::Observed<u64>,
) {
    observation_state(out, printer, name, observation);
    if let Some(value) = observation
        .value
        .filter(|_| observation.state == crate::model::ValueState::Value)
    {
        metric(out, name, printer, value as f64);
    }
}
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_cast)] // libc uses different statvfs field widths across Pi targets.
fn filesystem_free(path: &std::path::Path) -> u64 {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(p) = CString::new(path.as_os_str().as_bytes()) else {
        return 0;
    };
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(p.as_ptr(), &mut s) } == 0 {
        (s.f_bavail as u64).saturating_mul(s.f_frsize as u64)
    } else {
        0
    }
}
#[cfg(not(target_os = "linux"))]
fn filesystem_free(_: &std::path::Path) -> u64 {
    0
}
