use crate::device::DeviceProfile;
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::Deserialize;
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverDescriptor {
    pub id: &'static str,
    pub accepted_mime_types: &'static [&'static str],
    pub available: bool,
    pub device_configuration: bool,
    pub status_probe: bool,
}

pub const DRIVERS: &[DriverDescriptor] = &[DriverDescriptor {
    id: "zpl",
    accepted_mime_types: &[
        "application/zpl",
        "application/vnd.printhub.raster-page+json",
    ],
    available: true,
    device_configuration: true,
    status_probe: true,
}];

pub fn descriptor(id: &str) -> Option<&'static DriverDescriptor> {
    DRIVERS.iter().find(|driver| driver.id == id)
}

impl DriverDescriptor {
    pub fn accepts(&self, content_type: &str) -> bool {
        let mime_type = content_type.split(';').next().unwrap_or("").trim();
        self.accepted_mime_types.contains(&mime_type)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct PreparedPayload {
    pub bytes: Vec<u8>,
    pub label_count: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RasterPageV1 {
    version: u8,
    width_px: u32,
    height_px: u32,
    dpi: u32,
    copies: u16,
    black_bits_base64: String,
}

pub fn prepare_zebra_payload(
    content_type: &str,
    source: &[u8],
    profile: &DeviceProfile,
) -> Result<PreparedPayload> {
    match content_type.split(';').next().unwrap_or("").trim() {
        "application/zpl" => Ok(PreparedPayload {
            bytes: source.to_vec(),
            label_count: None,
        }),
        "application/vnd.printhub.raster-page+json" => encode_raster_page(source, profile),
        value => anyhow::bail!("unsupported Zebra payload type {value:?}"),
    }
}

fn encode_raster_page(source: &[u8], profile: &DeviceProfile) -> Result<PreparedPayload> {
    let page: RasterPageV1 =
        serde_json::from_slice(source).context("invalid PrintHub raster page JSON")?;
    ensure!(page.version == 1, "unsupported raster page version");
    ensure!(
        page.width_px > 0 && page.height_px > 0,
        "raster dimensions must be positive"
    );
    ensure!(
        page.copies > 0 && page.copies <= 999,
        "raster copies must be between 1 and 999"
    );
    ensure!(
        page.width_px <= profile.max_width_dots,
        "raster width exceeds printer profile"
    );
    ensure!(
        page.height_px <= profile.max_length_dots,
        "raster height exceeds printer profile"
    );
    if let Some(dpi) = profile.resolution_dpi {
        ensure!(page.dpi == dpi, "raster DPI does not match printer profile");
    }
    let bytes_per_row = usize::try_from(page.width_px.div_ceil(8))?;
    let expected = bytes_per_row
        .checked_mul(usize::try_from(page.height_px)?)
        .context("raster dimensions overflow")?;
    let packed = STANDARD
        .decode(page.black_bits_base64.as_bytes())
        .context("invalid raster base64")?;
    ensure!(
        packed.len() == expected,
        "raster byte count does not match dimensions"
    );
    let used_bits = page.width_px % 8;
    if used_bits != 0 {
        let unused_mask = (1u8 << (8 - used_bits)) - 1;
        ensure!(
            packed
                .chunks_exact(bytes_per_row)
                .all(|row| row[bytes_per_row - 1] & unused_mask == 0),
            "unused raster row bits must be white"
        );
    }
    let mut hexadecimal = String::with_capacity(packed.len() * 2);
    for byte in &packed {
        write!(&mut hexadecimal, "{byte:02X}").expect("writing to String cannot fail");
    }
    let zpl = format!(
        "^XA\n^PW{}\n^LL{}\n^FO0,0\n^GFA,{expected},{expected},{bytes_per_row},{hexadecimal}\n^FS\n^PQ{}\n^XZ\n",
        page.width_px, page.height_px, page.copies
    );
    Ok(PreparedPayload {
        bytes: zpl.into_bytes(),
        label_count: Some(u64::from(page.copies)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_keep_device_payload_contracts_explicit() {
        assert!(descriptor("zpl")
            .unwrap()
            .accepts("application/zpl; charset=utf-8"));
        assert!(descriptor("zpl")
            .unwrap()
            .accepts("application/vnd.printhub.raster-page+json"));
        assert!(descriptor("unknown").is_none());
    }

    fn profile(width: u32) -> DeviceProfile {
        DeviceProfile {
            resolution_dpi: Some(203),
            max_width_dots: width,
            max_length_dots: 100,
            ..DeviceProfile::default()
        }
    }

    #[test]
    fn raster_page_is_encoded_as_zebra_graphics_without_reprocessing_pixels() {
        let source = br#"{"version":1,"width_px":9,"height_px":2,"dpi":203,"copies":2,"black_bits_base64":"gACAAA=="}"#;
        let payload = prepare_zebra_payload(
            "application/vnd.printhub.raster-page+json",
            source,
            &profile(9),
        )
        .unwrap();
        assert_eq!(payload.label_count, Some(2));
        assert_eq!(
            String::from_utf8(payload.bytes).unwrap(),
            "^XA\n^PW9\n^LL2\n^FO0,0\n^GFA,4,4,2,80008000\n^FS\n^PQ2\n^XZ\n"
        );
    }

    #[test]
    fn raster_page_rejects_non_white_padding_and_profile_mismatches() {
        let padding = br#"{"version":1,"width_px":9,"height_px":1,"dpi":203,"copies":1,"black_bits_base64":"gAE="}"#;
        assert!(prepare_zebra_payload(
            "application/vnd.printhub.raster-page+json",
            padding,
            &profile(9)
        )
        .unwrap_err()
        .to_string()
        .contains("unused raster row bits"));

        let too_wide = br#"{"version":1,"width_px":9,"height_px":1,"dpi":203,"copies":1,"black_bits_base64":"gAA="}"#;
        assert!(prepare_zebra_payload(
            "application/vnd.printhub.raster-page+json",
            too_wide,
            &profile(8)
        )
        .unwrap_err()
        .to_string()
        .contains("width exceeds"));
    }
}
