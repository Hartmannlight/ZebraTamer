use crate::{device::DeviceProfile, driver::prepare_zebra_payload};
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const RASTER_MIME: &str = "application/vnd.printhub.raster-page+json";
pub const ZPL_MIME: &str = "application/zpl";
pub const MAX_ARTIFACTS: usize = 100;
pub const MAX_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_JOB_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitJob {
    pub idempotency_key: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub media_revision: Option<String>,
    #[serde(default = "one_copy")]
    pub copies: u16,
    #[serde(default)]
    pub reprint_of: Option<String>,
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub options: Value,
}

fn one_copy() -> u16 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub mime_type: String,
    pub sha256: String,
    pub data_base64: String,
}

#[derive(Debug)]
pub struct PreparedJob {
    pub bytes: Vec<u8>,
    pub request_hash: String,
    pub label_count: u64,
}

#[derive(Serialize)]
struct CanonicalRequest<'a> {
    printer_id: &'a str,
    copies: u16,
    media_revision: &'a Option<String>,
    reprint_of: &'a Option<String>,
    options: &'a Value,
    artifacts: Vec<CanonicalArtifact<'a>>,
}

#[derive(Serialize)]
struct CanonicalArtifact<'a> {
    mime_type: &'a str,
    sha256: &'a str,
}

pub fn prepare_job(
    printer_id: &str,
    request: &SubmitJob,
    profile: &DeviceProfile,
) -> Result<PreparedJob> {
    ensure!(
        !request.idempotency_key.is_empty() && request.idempotency_key.len() <= 255,
        "idempotency_key must contain between 1 and 255 characters"
    );
    ensure!(
        request
            .description
            .as_ref()
            .is_none_or(|value| value.len() <= 1000),
        "description is too long"
    );
    ensure!(
        (1..=999).contains(&request.copies),
        "copies must be between 1 and 999"
    );
    ensure!(
        !request.artifacts.is_empty() && request.artifacts.len() <= MAX_ARTIFACTS,
        "artifacts must contain between 1 and {MAX_ARTIFACTS} pages"
    );
    ensure!(
        request.options.is_null() || request.options.is_object(),
        "options must be an object"
    );

    let mut prepared_pages = Vec::with_capacity(request.artifacts.len());
    let mut canonical_artifacts = Vec::with_capacity(request.artifacts.len());
    let mut total_source_bytes = 0usize;
    let mut total_prepared_bytes = 0usize;
    for artifact in &request.artifacts {
        let mime_type = artifact.mime_type.split(';').next().unwrap_or("").trim();
        ensure!(
            matches!(mime_type, ZPL_MIME | RASTER_MIME),
            "unsupported artifact mime_type {:?}",
            artifact.mime_type
        );
        ensure!(
            artifact.sha256.len() == 64
                && artifact
                    .sha256
                    .bytes()
                    .all(|value| value.is_ascii_hexdigit()),
            "artifact sha256 must contain 64 hexadecimal characters"
        );
        let source = STANDARD
            .decode(artifact.data_base64.as_bytes())
            .context("invalid artifact base64")?;
        ensure!(
            source.len() <= MAX_ARTIFACT_BYTES,
            "artifact exceeds the maximum decoded size"
        );
        total_source_bytes = total_source_bytes
            .checked_add(source.len())
            .context("job size overflow")?;
        ensure!(
            total_source_bytes <= MAX_JOB_BYTES,
            "job exceeds the maximum decoded size"
        );
        let actual_hash = format!("{:x}", Sha256::digest(&source));
        ensure!(
            actual_hash.eq_ignore_ascii_case(&artifact.sha256),
            "artifact sha256 mismatch"
        );
        if mime_type == RASTER_MIME {
            let value: Value =
                serde_json::from_slice(&source).context("invalid PrintHub raster page JSON")?;
            ensure!(
                value.get("copies").and_then(Value::as_u64) == Some(1),
                "v2 raster artifacts must use copies=1; use envelope copies"
            );
        } else if request.copies > 1 {
            ensure!(
                !has_multi_copy_pq(&source),
                "native ZPL with ^PQ greater than 1 cannot be combined with envelope copies"
            );
        }
        let prepared = prepare_zebra_payload(mime_type, &source, profile)?;
        total_prepared_bytes = total_prepared_bytes
            .checked_add(prepared.bytes.len())
            .context("prepared job size overflow")?;
        ensure!(
            total_prepared_bytes
                .checked_mul(usize::from(request.copies))
                .is_some_and(|value| value <= MAX_JOB_BYTES),
            "prepared job exceeds the maximum size"
        );
        prepared_pages.push(prepared.bytes);
        canonical_artifacts.push(CanonicalArtifact {
            mime_type,
            sha256: &artifact.sha256,
        });
    }

    // Collated semantics: A,B,A,B rather than A,A,B,B.
    let mut bytes = Vec::with_capacity(total_prepared_bytes * usize::from(request.copies));
    for _ in 0..request.copies {
        for page in &prepared_pages {
            bytes.extend_from_slice(page);
        }
    }
    let canonical = CanonicalRequest {
        printer_id,
        copies: request.copies,
        media_revision: &request.media_revision,
        reprint_of: &request.reprint_of,
        options: &request.options,
        artifacts: canonical_artifacts,
    };
    let request_hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&canonical)?));
    Ok(PreparedJob {
        bytes,
        request_hash,
        label_count: u64::try_from(request.artifacts.len())? * u64::from(request.copies),
    })
}

fn has_multi_copy_pq(bytes: &[u8]) -> bool {
    bytes.windows(3).enumerate().any(|(at, marker)| {
        if marker != b"^PQ" {
            return false;
        }
        let digits: Vec<u8> = bytes[at + 3..]
            .iter()
            .copied()
            .take_while(u8::is_ascii_digit)
            .collect();
        std::str::from_utf8(&digits)
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|copies| copies > 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::json;

    fn artifact(mime_type: &str, source: &[u8]) -> Artifact {
        Artifact {
            mime_type: mime_type.into(),
            sha256: format!("{:x}", Sha256::digest(source)),
            data_base64: STANDARD.encode(source),
        }
    }

    fn request(artifacts: Vec<Artifact>) -> SubmitJob {
        SubmitJob {
            idempotency_key: "stable-request".into(),
            description: None,
            media_revision: None,
            copies: 2,
            reprint_of: None,
            artifacts,
            options: json!({}),
        }
    }

    #[test]
    fn prepares_multiple_pages_in_collated_copy_order() {
        let prepared = prepare_job(
            "printer",
            &request(vec![artifact(ZPL_MIME, b"A"), artifact(ZPL_MIME, b"B")]),
            &DeviceProfile::default(),
        )
        .unwrap();
        assert_eq!(prepared.bytes, b"ABAB");
        assert_eq!(prepared.label_count, 4);
    }

    #[test]
    fn validates_checksum_and_copy_ambiguity_before_preparing() {
        let mut bad_hash = request(vec![artifact(ZPL_MIME, b"^XA^XZ")]);
        bad_hash.artifacts[0].sha256 = "0".repeat(64);
        assert!(prepare_job("printer", &bad_hash, &DeviceProfile::default())
            .unwrap_err()
            .to_string()
            .contains("mismatch"));

        let ambiguous = request(vec![artifact(ZPL_MIME, b"^XA^PQ2^XZ")]);
        assert!(
            prepare_job("printer", &ambiguous, &DeviceProfile::default())
                .unwrap_err()
                .to_string()
                .contains("envelope copies")
        );
    }

    #[test]
    fn v2_raster_requires_envelope_copy_semantics() {
        let raster = br#"{"version":1,"width_px":8,"height_px":1,"dpi":203,"copies":2,"black_bits_base64":"gA=="}"#;
        let request = request(vec![artifact(RASTER_MIME, raster)]);
        assert!(prepare_job("printer", &request, &DeviceProfile::default())
            .unwrap_err()
            .to_string()
            .contains("copies=1"));
    }
}
