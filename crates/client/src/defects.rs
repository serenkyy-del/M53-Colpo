//! Physician-selected callouts. Raster bytes and clinical observations remain separate.
use m53_contracts::workflow::*;
pub use m53_domain::colpo::{PRESETS, Preset};
use m53_domain::{
    ExaminationState, FeatureState,
    protocol::*,
    validation::{Dictionary, TermKind},
};
use uuid::Uuid;
pub fn place(
    detail: &mut StudyDetail,
    dictionary: &Dictionary,
    preset: usize,
    asset: Uuid,
    x: f32,
    y: f32,
) -> Result<Uuid, String> {
    if detail.study.status == "approved" {
        return Err("Откройте новую редакцию".into());
    }
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err("Метка вне снимка".into());
    }
    if !detail
        .assets
        .iter()
        .any(|a| a.id == asset && a.kind == "original")
    {
        return Err("Снимок не принадлежит обследованию".into());
    }
    let p = PRESETS.get(preset).ok_or("Неизвестное обозначение")?;
    if !allows_marker(detail.annotations.iter().filter(|a| a.asset_id == asset), preset) {
        return Err("На одном снимке нельзя смешивать обозначения шейки и вульвы. Используйте другой снимок.".into());
    }
    if detail.protocol.as_ref().is_some_and(|p| p.colpo.is_some())
        && matches!(p.id, "cervix.symbol.6" | "cervix.symbol.11")
    {
        let ids:Vec<_>=detail.annotations.iter().filter(|a|a.asset_id==asset &&
            matches!(&a.layer,AnnotationLayer::Callout{full_name,..} if full_name==PRESETS[6].name || full_name==PRESETS[11].name))
            .filter(|a|a.label!=p.short).map(|a|a.id).collect();
        for id in ids {
            remove(detail, id);
        }
    }
    let id = Uuid::new_v4();
    let code = p.code.filter(|code| {
        dictionary
            .terms
            .iter()
            .any(|t| t.code == *code && t.kind == TermKind::Finding)
    });
    let mut finding_id = None;
    if let Some(code) = code {
        let term = dictionary.terms.iter().find(|t| t.code == code).unwrap();
        let protocol = detail.protocol.as_mut().ok_or("Нет протокола")?;
        let section = protocol
            .sections
            .iter_mut()
            .find(|s| s.region == term.region)
            .ok_or("Нет раздела обследования")?;
        if let Some(f) = section.findings.iter().find(|f| f.term_code == code) {
            if f.observation.value != FeatureState::Present {
                return Err(
                    "Этот признак уже указан с другим состоянием. Уточните его в протоколе.".into(),
                );
            }
            finding_id = Some(f.id);
        } else {
            let fid = Uuid::new_v4();
            finding_id = Some(fid);
            section.findings.push(Finding {
                id: fid,
                term_code: code.into(),
                observation: Field {
                    value: FeatureState::Present,
                    origin: Origin::Annotation { annotation_id: id },
                },
                physician_confirmed: true,
            });
            section.state = Field {
                value: ExaminationState::Examined,
                origin: Origin::Physician,
            };
        }
    }
    detail.annotations.push(Annotation {
        id,
        asset_id: asset,
        lesion_id: None,
        finding_id,
        coordinate_system: Default::default(),
        layer: AnnotationLayer::Callout {
            end_x: x,
            end_y: y,
            full_name: p.name.into(),
            term_code: code.map(str::to_owned),
        },
        x: (x - 0.15).clamp(0.06, 0.94),
        y: (y - 0.12).clamp(0.06, 0.94),
        radius: 0.045,
        label: p.short.into(),
    });
    sync_colpo(detail);
    Ok(id)
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    Body,
    Tip,
    Radius,
}
/// Image-relative coordinates; radius uses the shorter oriented image dimension.
pub fn edit(a: &mut Annotation, handle: Handle, x: f32, y: f32, aspect: f32) {
    if !x.is_finite() || !y.is_finite() || !aspect.is_finite() || aspect <= 0.0 {
        return;
    }
    match handle {
        Handle::Body => {
            a.x = x.clamp(0.0, 1.0);
            a.y = y.clamp(0.0, 1.0);
        }
        Handle::Tip => {
            if let AnnotationLayer::Callout { end_x, end_y, .. } = &mut a.layer {
                *end_x = x.clamp(0.0, 1.0);
                *end_y = y.clamp(0.0, 1.0);
            }
        }
        Handle::Radius => {
            let sx = aspect.max(1.0);
            let sy = (1.0 / aspect).max(1.0);
            a.radius = ((x - a.x) * sx).hypot((y - a.y) * sy).clamp(0.02, 0.2);
        }
    }
}
/// Deleting a marker does not erase observations subsequently edited by the physician.
pub fn remove(detail: &mut StudyDetail, id: Uuid) {
    if detail.study.status == "approved" {
        return;
    }
    detail.annotations.retain(|a| a.id != id);
    if let Some(protocol) = detail.protocol.as_mut() {
        for s in &mut protocol.sections {
            for f in &mut s.findings {
                if matches!(f.observation.origin,Origin::Annotation{annotation_id} if annotation_id==id)
                    && let Some(remaining) = detail
                        .annotations
                        .iter()
                        .find(|a| a.finding_id == Some(f.id))
                {
                    f.observation.origin = Origin::Annotation {
                        annotation_id: remaining.id,
                    };
                }
            }
            s.findings.retain(|f| !matches!(f.observation.origin,Origin::Annotation{annotation_id} if annotation_id==id) || detail.annotations.iter().any(|a|a.finding_id==Some(f.id)));
        }
    }
    if let Some(protocol) = detail.protocol.as_mut() {
        for section in &mut protocol.sections {
            for lesion in &mut section.lesions {
                for f in &mut lesion.findings {
                    if matches!(f.observation.origin,Origin::Annotation{annotation_id} if annotation_id==id)
                        && let Some(remaining) = detail
                            .annotations
                            .iter()
                            .find(|a| a.finding_id == Some(f.id))
                    {
                        f.observation.origin = Origin::Annotation {
                            annotation_id: remaining.id,
                        };
                    }
                }
                lesion.findings.retain(|f|!matches!(f.observation.origin,Origin::Annotation{annotation_id} if annotation_id==id) || detail.annotations.iter().any(|a|a.finding_id==Some(f.id)));
            }
        }
    }
    sync_colpo(detail);
}
pub fn retag(
    detail: &mut StudyDetail,
    dictionary: &Dictionary,
    id: Uuid,
    preset: usize,
) -> Result<(), String> {
    if detail.study.status == "approved" {
        return Err("Откройте новую редакцию".into());
    }
    let old = detail
        .annotations
        .iter()
        .find(|a| a.id == id)
        .cloned()
        .ok_or("Метка не найдена")?;
    let AnnotationLayer::Callout { end_x, end_y, .. } = old.layer else {
        return Err("Не метка дефекта".into());
    };
    // Stage on a copy: a conflict with an existing negative finding must leave both objects unchanged.
    let mut staged = detail.clone();
    remove(&mut staged, id);
    let new_id = place(&mut staged, dictionary, preset, old.asset_id, end_x, end_y)?;
    let a = staged
        .annotations
        .iter_mut()
        .find(|a| a.id == new_id)
        .unwrap();
    a.x = old.x;
    a.y = old.y;
    a.radius = old.radius;
    a.id = id;
    if let Some(p) = staged.protocol.as_mut() {
        for s in &mut p.sections {
            for f in &mut s.findings {
                if matches!(f.observation.origin,Origin::Annotation{annotation_id} if annotation_id==new_id)
                {
                    f.observation.origin = Origin::Annotation { annotation_id: id };
                }
            }
        }
    }
    let mut new_finding = staged
        .annotations
        .iter()
        .find(|a| a.id == id)
        .and_then(|a| a.finding_id);
    if let Some(lesion_id) = old.lesion_id {
        let shared = staged
            .annotations
            .iter()
            .any(|a| a.id != id && a.finding_id == new_finding);
        if let Some(protocol) = staged.protocol.as_mut() {
            let mut moved = None;
            for section in &mut protocol.sections {
                if let Some(pos) = section
                    .findings
                    .iter()
                    .position(|f| Some(f.id) == new_finding)
                {
                    moved = Some(if shared {
                        let mut finding = section.findings[pos].clone();
                        finding.id = Uuid::new_v4();
                        finding.observation.origin = Origin::Annotation { annotation_id: id };
                        new_finding = Some(finding.id);
                        finding
                    } else {
                        section.findings.remove(pos)
                    });
                }
            }
            if let Some(lesion) = protocol
                .sections
                .iter_mut()
                .flat_map(|s| &mut s.lesions)
                .find(|l| l.id == lesion_id)
                && let Some(finding) = moved
            {
                if let Some(existing) = lesion
                    .findings
                    .iter()
                    .find(|f| f.term_code == finding.term_code)
                {
                    new_finding = Some(existing.id);
                } else {
                    lesion.findings.push(finding);
                }
            }
        }
        if let Some(a) = staged.annotations.iter_mut().find(|a| a.id == id) {
            a.lesion_id = Some(lesion_id);
            a.finding_id = new_finding;
        }
    }
    sync_colpo(&mut staged);
    *detail = staged;
    Ok(())
}

/// Rebuild only the derived symbol index; retain properties by annotation UUID.
pub fn sync_colpo(detail: &mut StudyDetail) {
    let Some(colpo) = detail.protocol.as_mut().and_then(|p| p.colpo.as_mut()) else {
        return;
    };
    let old = std::mem::take(&mut colpo.marks);
    for (id, mark) in &old {
        colpo
            .mark_history
            .insert(format!("{id}:{}", mark.catalog_id), mark.clone());
    }
    for a in &detail.annotations {
        if let AnnotationLayer::Callout { full_name, .. } = &a.layer
            && let Some(p) = PRESETS
                .iter()
                .find(|p| p.short == a.label && p.name == full_name)
        {
            let mut properties = old
                .get(&a.id)
                .filter(|m| m.catalog_id == p.id)
                .or_else(|| colpo.mark_history.get(&format!("{}:{}", a.id, p.id)))
                .map(|m| m.properties.clone())
                .unwrap_or_default();
            properties.retain(|key, v| {
                m53_domain::colpo::FIELDS.iter().any(|f| {
                    f.id == key && f.parent == p.id && m53_domain::colpo::validate_value(f, v)
                })
            });
            colpo.marks.insert(
                a.id,
                m53_domain::colpo::Mark {
                    asset_id: a.asset_id,
                    catalog_id: p.id.into(),
                    properties,
                },
            );
        }
    }
    let regions: Vec<_> = colpo
        .marks
        .values()
        .filter_map(|m| m53_domain::colpo::preset(&m.catalog_id).map(|p| p.region))
        .collect();
    if let Some(protocol) = &mut detail.protocol {
        for section in &mut protocol.sections {
            if regions.contains(&section.region) {
                section.state = Field {
                    value: ExaminationState::Examined,
                    origin: Origin::Physician,
                };
            }
        }
    }
}

/// Consistent image-relative typography; only small badges or long labels shrink.
pub fn callout_font_size(short_side: f32, radius: f32, label: &str) -> f32 {
    (short_side * 0.026).min(radius * 0.95)
        .min(radius * 3.5 / label.chars().count().max(1) as f32).max(1.)
}

/// Hidden markers still belong to the photograph. Unknown legacy objects are preserved.
pub fn allows_marker<'a>(annotations: impl IntoIterator<Item = &'a Annotation>, preset: usize) -> bool {
    let Some(requested) = PRESETS.get(preset) else { return false; };
    annotations.into_iter().all(|a| {
        let AnnotationLayer::Callout { full_name, .. } = &a.layer else { return true; };
        PRESETS.iter().find(|p| p.short == a.label && p.name == full_name)
            .is_none_or(|p| (p.region == Region::Cervix) == (requested.region == Region::Cervix))
    })
}

/// Fixed badge radius relative to the shorter original image dimension.
pub const CALLOUT_RADIUS: f32 = 0.045;
