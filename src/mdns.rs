use crate::config::Config;
use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;

pub struct MdnsGuard {
    _daemon: ServiceDaemon,
}

pub fn announce(config: &Config) -> Result<Option<MdnsGuard>> {
    if !config.mdns_enabled {
        return Ok(None);
    }
    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
    let host = hostname();
    let port = config.listen.port();
    let mut agent_props: HashMap<String, String> = HashMap::new();
    agent_props.insert("api_version".into(), "v1".into());
    agent_props.insert("rest_path".into(), "/v1".into());
    agent_props.insert("metrics_path".into(), "/metrics".into());
    let agent = ServiceInfo::new(
        "_zpl-agent._tcp.local.",
        "zpl-agent",
        &host,
        "0.0.0.0",
        port,
        agent_props,
    )?
    .enable_addr_auto();
    daemon.register(agent)?;
    for p in &config.printers {
        let mut props = HashMap::new();
        props.insert("api_version".to_string(), "v1".to_string());
        props.insert("rest_path".to_string(), format!("/v1/printers/{}", p.id));
        props.insert(
            "snapshot_path".to_string(),
            format!("/v1/printers/{}/snapshot", p.id),
        );
        props.insert("metrics_path".to_string(), "/metrics".to_string());
        props.insert("printer_id".to_string(), p.id.clone());
        props.insert("transport".to_string(), p.transport.clone());
        let info = ServiceInfo::new(
            "_zpl-printer._tcp.local.",
            &p.id,
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
fn hostname() -> String {
    let name = std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zpl-agent".into());
    if name.ends_with('.') {
        name
    } else if name.ends_with(".local") {
        format!("{name}.")
    } else {
        format!("{name}.local.")
    }
}
