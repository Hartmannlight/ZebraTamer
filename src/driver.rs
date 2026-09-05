#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverDescriptor {
    pub id: &'static str,
    pub accepted_mime_types: &'static [&'static str],
    pub available: bool,
    pub device_configuration: bool,
    pub status_probe: bool,
}

pub const DRIVERS: &[DriverDescriptor] = &[
    DriverDescriptor {
        id: "zpl",
        accepted_mime_types: &["application/zpl"],
        available: true,
        device_configuration: true,
        status_probe: true,
    },
    DriverDescriptor {
        id: "niimbot_b1",
        accepted_mime_types: &["application/vnd.printhub.niimbot-b1-raster+v1"],
        available: false,
        device_configuration: false,
        status_probe: false,
    },
];

pub fn descriptor(id: &str) -> Option<&'static DriverDescriptor> {
    DRIVERS.iter().find(|driver| driver.id == id)
}

impl DriverDescriptor {
    pub fn accepts(&self, content_type: &str) -> bool {
        let mime_type = content_type.split(';').next().unwrap_or("").trim();
        self.accepted_mime_types.contains(&mime_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_keep_device_payload_contracts_explicit() {
        assert!(descriptor("zpl")
            .unwrap()
            .accepts("application/zpl; charset=utf-8"));
        let niimbot = descriptor("niimbot_b1").unwrap();
        assert!(!niimbot.available);
        assert!(niimbot.accepts("application/vnd.printhub.niimbot-b1-raster+v1"));
        assert!(descriptor("unknown").is_none());
    }
}
