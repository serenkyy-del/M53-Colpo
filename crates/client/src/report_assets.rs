//! Bounded background preparation of exactly the report's selected originals.
use crate::{connection::Connection, report::Photo};
use m53_contracts::workflow::Asset;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};
use uuid::Uuid;

pub fn load_selected(
    connection: &Connection,
    assets: &[Asset],
    selected: &[Uuid],
) -> Result<Vec<Photo>, String> {
    load_until(
        connection,
        assets,
        selected,
        Instant::now() + Duration::from_secs(45),
        || false,
    )
}

/// Call on a report worker, never on the UI/clinical command loop. Cancellation
/// stops waiting/rendering; already requested immutable originals may finish caching.
pub fn load_until(
    connection: &Connection,
    assets: &[Asset],
    selected: &[Uuid],
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<Vec<Photo>, String> {
    let aliases = connection
        .offline
        .as_ref()
        .map(|o| o.identifier_aliases())
        .unwrap_or_default();
    let canonical = |mut id: Uuid| {
        for _ in 0..aliases.len() {
            let Some(next) = aliases.get(&id) else {
                break;
            };
            id = *next;
        }
        id
    };
    let selected: BTreeSet<_> = selected.iter().map(|id| canonical(*id)).collect();
    let mut seen = BTreeSet::new();
    let originals: Vec<_> = assets
        .iter()
        .filter(|asset| asset.kind == "original")
        .enumerate()
        .filter(|(_, asset)| {
            selected.contains(&canonical(asset.id)) && seen.insert(canonical(asset.id))
        })
        .collect();
    let mut ready = BTreeMap::new();
    // Queue every selected miss before waiting, so the shared two-file pool can
    // transfer them concurrently. A cached original never causes another GET.
    for (_, asset) in &originals {
        ensure_active(deadline, &cancelled)?;
        let bytes = if connection.offline.is_some() {
            connection.prepare_asset(asset.id)?
        } else {
            let bytes = connection
                .download_timed(asset.id, deadline.saturating_duration_since(Instant::now()))?;
            use sha2::Digest;
            if hex::encode(sha2::Sha256::digest(bytes.as_ref())) != asset.sha256 {
                return Err("SHA-256 выбранного снимка не совпадает; отчет не сформирован".into());
            }
            Some(bytes)
        };
        if let Some(bytes) = bytes {
            ready.insert(asset.id, bytes);
        }
    }
    while ready.len() < originals.len() {
        ensure_active(deadline, &cancelled)?;
        for (_, asset) in &originals {
            if ready.contains_key(&asset.id) {
                continue;
            }
            if let Some(bytes) = connection.prepare_asset(asset.id)? {
                ready.insert(asset.id, bytes);
            }
        }
        if ready.len() < originals.len() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    ensure_active(deadline, &cancelled)?;
    Ok(originals
        .into_iter()
        .map(|(index, asset)| Photo {
            id: asset.id,
            bytes: Arc::unwrap_or_clone(ready.remove(&asset.id).unwrap()),
            caption: format!("Снимок {}", index + 1),
        })
        .collect())
}

fn ensure_active(deadline: Instant, cancelled: &impl Fn() -> bool) -> Result<(), String> {
    if cancelled() {
        return Err("Подготовка отчета отменена".into());
    }
    if Instant::now() >= deadline {
        return Err(
            "Выбранные снимки не загрузились за 45 секунд; повторите подготовку отчета".into(),
        );
    }
    Ok(())
}
