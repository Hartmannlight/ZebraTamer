use crate::config::Config;
use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub struct MdnsGuard {
    _daemon: ServiceDaemon,
}

pub fn announce(config: &Config) -> Result<Option<MdnsGuard>> {
    if !config.mdns_enabled {
        return Ok(None);
    }
    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
    let port = config.listen.port();
    let mut agent_props: HashMap<String, String> = HashMap::new();
    let agent_id = config
        .agent_id
        .as_deref()
        .context("agent identity must be initialized before mDNS")?;
    let host = hostname(agent_id);
    agent_props.insert("agent_id".into(), agent_id.into());
    agent_props.insert("api_version".into(), "v1".into());
    agent_props.insert("rest_path".into(), "/v1".into());
    agent_props.insert("metrics_path".into(), "/metrics".into());
    let agent = ServiceInfo::new(
        "_print-agent._tcp.local.",
        &instance_name("print-agent", agent_id),
        &host,
        "0.0.0.0",
        port,
        agent_props,
    )?
    .enable_addr_auto();
    daemon.register(agent)?;
    for p in &config.printers {
        let mut props = HashMap::new();
        props.insert("agent_id".to_string(), agent_id.to_string());
        props.insert("api_version".to_string(), "v1".to_string());
        props.insert("rest_path".to_string(), format!("/v1/printers/{}", p.id));
        props.insert(
            "snapshot_path".to_string(),
            format!("/v1/printers/{}/snapshot", p.id),
        );
        props.insert("metrics_path".to_string(), "/metrics".to_string());
        props.insert("printer_id".to_string(), p.id.clone());
        props.insert("transport".to_string(), p.transport.clone());
        props.insert("driver".to_string(), p.driver.clone());
        let info = ServiceInfo::new(
            "_print-agent-printer._tcp.local.",
            &instance_name("print-agent-printer", &format!("{agent_id}:{}", p.id)),
            &host,
            "0.0.0.0",
            port,
            props,
        )?
        .enable_addr_auto();
        daemon.register(info)?;
    }
    Ok(Some(MdnsGuard { _daemon: daemon }))
}
fn instance_name(prefix: &str, identity: &str) -> String {
    // DNS labels are limited to 63 octets, regardless of configured ID length.
    let digest = Sha256::digest(identity.as_bytes());
    let suffix: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{prefix}-{suffix}")
}

fn hostname(agent_id: &str) -> String {
    let name = std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| instance_name("print-agent", agent_id));
    if name.ends_with('.') {
        name
    } else if name.ends_with(".local") {
        format!("{name}.")
    } else {
        format!("{name}.local.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_names_are_short_stable_and_agent_specific() {
        let long = "x".repeat(200);
        assert!(instance_name("zpl-printer", &long).len() <= 63);
        assert_eq!(
            instance_name("print-agent", &long),
            instance_name("print-agent", &long)
        );
        assert_ne!(
            instance_name("print-agent-printer", "pi-a:zebra"),
            instance_name("print-agent-printer", "pi-b:zebra")
        );
    }
}
