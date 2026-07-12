use crate::{
    api::AppState,
    model::{AccountingConfidence, ValueState},
};
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
        metric(
            &mut out,
            "zpl_printer_present",
            &p,
            bool_obs(&s.transport.present),
        );
        metric(
            &mut out,
            "zpl_printer_open",
            &p,
            bool_obs(&s.transport.open),
        );
        metric(
            &mut out,
            "zpl_printer_protocol_up",
            &p,
            bool_obs(&s.transport.protocol_up),
        );
        metric(&mut out, "zpl_printer_ready", &p, bool_obs(&s.status.ready));
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
            let value = match fault {
                "paused" => bool_obs(&s.status.paused),
                "media_out" => bool_obs(&s.status.media_out),
                "ribbon_out" => bool_obs(&s.status.ribbon_out),
                "head_open" => bool_obs(&s.status.head_open),
                _ => bool_obs(&s.status.temperature_fault),
            };
            let _ = writeln!(
                out,
                "zpl_printer_fault{{printer=\"{p}\",fault=\"{fault}\"}} {value}"
            );
        }
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
fn bool_obs(v: &crate::model::Observed<bool>) -> f64 {
    if v.state == ValueState::Value && v.value == Some(true) {
        1.0
    } else {
        0.0
    }
}
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
#[cfg(target_os = "linux")]
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
