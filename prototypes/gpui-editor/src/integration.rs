//! Native editor -> clinical model. Mutations are staged and retain physician-owned findings.
use crate::Session;
use m53_client::{
    defects::{self, PRESETS},
    workspace::Workspace,
};
use m53_contracts::workflow::*;
use m53_domain::{protocol::Origin, validation::Dictionary};
use std::collections::BTreeSet;
use uuid::Uuid;

pub fn apply(
    detail: &StudyDetail,
    dictionary: &Dictionary,
    session: &Session,
) -> Result<StudyDetail, String> {
    let assets: BTreeSet<_> = detail
        .assets
        .iter()
        .filter(|a| a.kind == "original")
        .map(|a| a.id)
        .collect();
    let mut photo_ids = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for p in &session.photos {
        if !assets.contains(&p.id) || !photo_ids.insert(p.id) {
            return Err("Снимок не принадлежит исследованию".into());
        }
        if p.removed {continue;}
        for a in &p.annotations {
            if !ids.insert(a.id)
                || a.id.is_nil()
                || a.asset_id != p.id
                || !a.valid_layer()
                || !a.x.is_finite()
                || !a.y.is_finite()
                || !a.radius.is_finite()
                || !(0.0..=1.0).contains(&a.x)
                || !(0.0..=1.0).contains(&a.y)
                || !(0.0..=1.0).contains(&a.radius)
            {
                return Err("Некорректная геометрия разметки".into());
            }
        }
    }
    if detail.study.status == "approved" {
        for p in &session.photos {
            if p.removed {return Err("Удаление снимков доступно только в черновике".into());}
            let old: Vec<_> = detail
                .annotations
                .iter()
                .filter(|a| a.asset_id == p.id)
                .collect();
            if serde_json::json!(old) != serde_json::json!(p.annotations) {
                return Err("Утвержденный протокол неизменяем; откройте новую редакцию".into());
            }
        }
        return Ok(detail.clone());
    }
    let mut staged = detail.clone();
    for a in detail
        .annotations
        .iter()
        .filter(|a| photo_ids.contains(&a.asset_id) && !ids.contains(&a.id))
    {
        defects::remove(&mut staged, a.id);
    }
    for incoming in session.photos.iter().filter(|p|!p.removed).flat_map(|p| &p.annotations) {
        let old = staged
            .annotations
            .iter()
            .find(|a| a.id == incoming.id)
            .cloned();
        if old
            .as_ref()
            .is_some_and(|a| a.asset_id != incoming.asset_id)
        {
            return Err("Идентификатор метки относится к другому снимку".into());
        }
        if let AnnotationLayer::Callout {
            end_x,
            end_y,
            ref full_name,
            ..
        } = incoming.layer
        {
            let preset = PRESETS
                .iter()
                .position(|p| p.short == incoming.label && p.name == full_name)
                .ok_or("Неизвестное обозначение дефекта")?;
            if old.is_none() {
                let generated = defects::place(
                    &mut staged,
                    dictionary,
                    preset,
                    incoming.asset_id,
                    end_x,
                    end_y,
                )?;
                staged
                    .annotations
                    .iter_mut()
                    .find(|a| a.id == generated)
                    .unwrap()
                    .id = incoming.id;
                if let Some(protocol) = &mut staged.protocol {
                    for s in &mut protocol.sections {
                        for f in &mut s.findings {
                            if matches!(f.observation.origin,Origin::Annotation{annotation_id} if annotation_id==generated)
                            {
                                f.observation.origin = Origin::Annotation {
                                    annotation_id: incoming.id,
                                };
                            }
                        }
                    }
                }
            } else if old.as_ref().is_some_and(|a| a.label != incoming.label || matches!(&a.layer,AnnotationLayer::Callout{full_name:old,..} if old!=full_name)) {
                defects::retag(&mut staged, dictionary, incoming.id, preset)?;
            }
            let a = staged
                .annotations
                .iter_mut()
                .find(|a| a.id == incoming.id)
                .unwrap();
            a.x = incoming.x;
            a.y = incoming.y;
            a.radius = incoming.radius;
            if let AnnotationLayer::Callout {
                end_x: x, end_y: y, ..
            } = &mut a.layer
            {
                *x = end_x;
                *y = end_y;
            } else {
                return Err("Тип слоя не совпадает".into());
            }
        } else if let Some(a) = staged.annotations.iter_mut().find(|a| a.id == incoming.id) {
            if std::mem::discriminant(&a.layer) != std::mem::discriminant(&incoming.layer) {
                return Err("Тип слоя не совпадает".into());
            }
            a.x = incoming.x;
            a.y = incoming.y;
            a.radius = incoming.radius;
            a.layer = incoming.layer.clone();
        } else {
            if !matches!(
                incoming.layer,
                AnnotationLayer::Target { .. } | AnnotationLayer::OutputRegion { .. }
            ) {
                return Err("Новый слой не поддерживается".into());
            }
            let mut a = incoming.clone();
            a.finding_id = None;
            a.lesion_id = None;
            staged.annotations.push(a);
        }
    }
    defects::sync_colpo(&mut staged);
    Ok(staged)
}
pub fn checkpoint(workspace: &mut Workspace, study: Uuid, session: &Session) -> Result<(), String> {
    let detail = workspace.detail.as_ref().ok_or("Нет исследования")?;
    if detail.study.id != study {
        return Err("Редактор относится к другому исследованию".into());
    }
    let mut input=session.clone();
    if detail.study.status=="approved" {
        let previous:Session=serde_json::from_value(workspace.journal["native_editor"].clone()).unwrap_or_default();
        let removed=|s:&Session|s.photos.iter().filter(|p|p.removed).map(|p|p.id).collect::<BTreeSet<_>>();
        if removed(&previous)!=removed(session){return Err("Удаление снимков доступно только в черновике".into());}
        input.photos.retain(|p|!p.removed);
    }
    let staged = match apply(
        detail,
        workspace.dictionary.as_ref().ok_or("Словарь не загружен")?,
        &input,
    ) {
        Ok(detail) => detail,
        Err(error) => {
            workspace.journal["native_editor_rejected"] =
                serde_json::json!({"study":study,"session":session});
            workspace.persist();
            return Err(error);
        }
    };
    let mut journal = workspace.journal.clone();
    journal
        .as_object_mut()
        .unwrap()
        .remove("native_editor_rejected");
    crate::state::set_detail(&mut journal, &staged);
    journal["selected_photos"] = serde_json::json!(
        session
            .photos
            .iter()
            .filter(|p| p.in_report && !p.removed)
            .map(|p| p.id)
            .collect::<Vec<_>>()
    );
    journal["native_editor"] = serde_json::json!(session);
    workspace.checkpoint(journal)
}

/// Removal is local and reversible in the editor journal; original blobs remain immutable.
pub fn visible_detail(detail:&StudyDetail,journal:&serde_json::Value)->StudyDetail {
    let session:Session=serde_json::from_value(journal["native_editor"].clone()).unwrap_or_default();
    let removed:BTreeSet<_>=session.photos.iter().filter(|p|p.removed).map(|p|p.id).collect();
    let mut visible=detail.clone();visible.assets.retain(|a|!removed.contains(&a.id));visible.annotations.retain(|a|!removed.contains(&a.asset_id));visible
}
