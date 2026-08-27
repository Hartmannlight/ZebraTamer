//! The agent owns media metadata. All mutations run in the printer worker,
//! including edits made while the physical printer is disconnected.
use crate::{device::revision, model::*, persist::Store};
use anyhow::{ensure, Result};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditRequest {
    pub revision: String,
    pub media: MediaDefinition,
}

pub fn media_revision(media: &MediaState) -> String {
    // Consumption must not make an otherwise unchanged media editor stale.
    revision(&(media.loaded_at, &media.media))
}

pub fn validate(media: &MediaDefinition) -> Result<()> {
    ensure!(
        !media.display_name.trim().is_empty() && media.display_name.len() <= 200,
        "Media name is required (max 200 characters)"
    );
    for n in [media.width_mm, media.height_mm] {
        ensure!(
            n.is_finite() && n > 0.0 && n <= 10000.0,
            "Media dimensions must be finite and between 0 and 10000 mm"
        );
    }
    for n in [
        media.gap_mm,
        media.corner_radius_mm,
        media.black_mark_width_mm,
        media.black_mark_height_mm,
        media.core_diameter_mm,
        media.outer_diameter_mm,
        media.thickness_micrometers,
    ]
    .into_iter()
    .flatten()
    {
        ensure!(
            n.is_finite() && (0.0..=100000.0).contains(&n),
            "Invalid media measurement"
        );
    }
    ensure!(
        !media.color.name.trim().is_empty() && media.color.name.len() <= 100,
        "Color name is required (max 100 characters)"
    );
    if let Some(hex) = &media.color.hex {
        ensure!(
            hex.len() == 7
                && hex.starts_with('#')
                && hex[1..].bytes().all(|c| c.is_ascii_hexdigit()),
            "Color hex must be #RRGGBB"
        );
    }
    ensure!(
        media.labels_available_at_load <= i64::MAX as u64,
        "Label count is too large"
    );
    Ok(())
}

fn archive(store: &Store, id: &str, state: &MediaState) -> Result<()> {
    crate::persist::atomic_json(
        &store
            .printer_dir(id)
            .join(format!("media-{}.json", uuid::Uuid::new_v4())),
        state,
    )
}

pub fn load(store: &Store, id: &str, definition: MediaDefinition) -> Result<Value> {
    validate(&definition)?;
    if let Some(old) = store.load_media(id)? {
        archive(store, id, &old)?;
    }
    let state = MediaState {
        initial_labels: definition.labels_available_at_load,
        remaining_labels: definition.labels_available_at_load,
        consumed_labels_total: 0,
        accounting_deficit_labels: 0,
        accounting_confidence: AccountingConfidence::Estimated,
        last_accounting_event_at: None,
        ledger_sequence: 0,
        loaded_at: now(),
        media: definition,
    };
    store.save_media(id, &state)?;
    Ok(serde_json::to_value(state)?)
}

pub fn edit(store: &Store, id: &str, request: EditRequest) -> Result<Value> {
    validate(&request.media)?;
    let mut state = store
        .load_media(id)?
        .ok_or_else(|| anyhow::anyhow!("No media loaded"))?;
    ensure!(
        media_revision(&state) == request.revision,
        "conflict: Loaded media changed; refresh before saving"
    );
    state.media = request.media;
    // Editing color, size or name is not loading a new roll and never resets accounting.
    state.media.labels_available_at_load = state.initial_labels;
    store.save_media(id, &state)?;
    Ok(serde_json::to_value(state)?)
}

pub fn unload(store: &Store, id: &str) -> Result<Value> {
    let state = store
        .load_media(id)?
        .ok_or_else(|| anyhow::anyhow!("No media loaded"))?;
    archive(store, id, &state)?;
    std::fs::remove_file(store.media_path(id))?;
    Ok(json!({"unloaded": true}))
}

pub fn adjust(store: &Store, id: &str, adj: MediaAdjustment) -> Result<Value> {
    let mut state = store
        .load_media(id)?
        .ok_or_else(|| anyhow::anyhow!("No media loaded"))?;
    let before = state.remaining_labels;
    if adj.delta < 0 {
        let debit = adj.delta.unsigned_abs();
        state.remaining_labels = before.saturating_sub(debit);
        state.consumed_labels_total = state.consumed_labels_total.saturating_add(debit);
        state.accounting_deficit_labels = state
            .accounting_deficit_labels
            .saturating_add(debit.saturating_sub(before));
    } else {
        state.remaining_labels = before.saturating_add(adj.delta as u64);
    }
    state.ledger_sequence += 1;
    state.last_accounting_event_at = Some(now());
    store.append_media_ledger(
        id,
        &MediaLedgerEvent {
            sequence: state.ledger_sequence,
            at: now(),
            delta: adj.delta,
            reason: adj.reason,
            source: adj.source,
            job_id: adj.job_id,
            before,
            after: state.remaining_labels,
            deficit_after: state.accounting_deficit_labels,
            hardware_counter_before: adj.hardware_counter_before,
            hardware_counter_after: adj.hardware_counter_after,
        },
    )?;
    store.save_media(id, &state)?;
    Ok(serde_json::to_value(state)?)
}

pub fn state_with_revision(store: &Store, id: &str) -> Result<Value> {
    let state = store.load_media(id)?;
    Ok(json!({"state": state, "revision": state.as_ref().map(media_revision)}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn definition() -> MediaDefinition {
        serde_json::from_value(
            json!({"display_name":"Yellow labels", "width_mm":50, "height_mm":25,
            "shape":"rectangle", "tracking":"gap", "print_technology":"direct_thermal",
            "color":{"name":"Yellow", "hex":"#ffdd00"}, "preferred_settings":{},
            "labels_available_at_load":100, "low_warning_threshold":10}),
        )
        .unwrap()
    }
    #[test]
    fn metadata_edit_preserves_consumption_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().into()).unwrap();
        load(&store, "p", definition()).unwrap();
        let revision = media_revision(&store.load_media("p").unwrap().unwrap());
        adjust(
            &store,
            "p",
            MediaAdjustment {
                delta: -3,
                reason: "test".into(),
                source: "test".into(),
                job_id: None,
                hardware_counter_before: None,
                hardware_counter_after: None,
            },
        )
        .unwrap();
        let mut changed = definition();
        changed.color.name = "Blue".into();
        changed.color.hex = Some("#0000ff".into());
        edit(
            &store,
            "p",
            EditRequest {
                revision: revision.clone(),
                media: changed,
            },
        )
        .unwrap();
        let reopened = Store::open(dir.path().into()).unwrap();
        let state = reopened.load_media("p").unwrap().unwrap();
        assert_eq!(state.remaining_labels, 97);
        assert_eq!(state.consumed_labels_total, 3);
        assert_eq!(state.media.color.name, "Blue");
        assert!(edit(
            &store,
            "p",
            EditRequest {
                revision,
                media: definition()
            }
        )
        .is_err());
        unload(&store, "p").unwrap();
        assert!(store.load_media("p").unwrap().is_none());
    }
}
