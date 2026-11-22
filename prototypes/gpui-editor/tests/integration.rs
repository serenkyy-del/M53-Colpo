#[path = "../src/verification.rs"]
mod fixture;
use m53_domain::FeatureState;
use m53_domain::protocol::*;
use m53_gpui_editor::{Photo, Session, integration, schema};
use serde_json::json;
fn session(photo: Photo) -> Session {
    Session {
        photos: vec![photo],
        selected: 0,
    }
}
#[test]
fn clinical_callout_edit_retag_and_delete_preserve_identity_and_manual_findings() {
    let (detail, dictionary, asset) = fixture::fixture();
    let mut p = Photo::new(1000, 800);
    p.id = asset;
    let id = p.add_callout(7, 0.2, 0.3);
    let first = integration::apply(&detail, &dictionary, &session(p.clone())).unwrap();
    let fid = first.annotations[0].finding_id.unwrap();
    assert_eq!(
        first.protocol.as_ref().unwrap().sections[0].findings.len(),
        1
    );
    p.annotations[0].x = 0.4;
    p.annotations[0].radius = 0.07;
    let moved = integration::apply(&first, &dictionary, &session(p.clone())).unwrap();
    assert_eq!(moved.annotations[0].finding_id, Some(fid));
    assert_eq!(moved.annotations[0].id, id);
    let mut manual = moved.clone();
    let mut protocol = json!(manual.protocol);
    schema::set(
        &mut protocol,
        "/sections/0/findings/0/term_code",
        json!("cervix.major.dense_acetowhite_epithelium"),
    )
    .unwrap();
    manual.protocol = Some(serde_json::from_value(protocol).unwrap());
    assert!(matches!(
        manual.protocol.as_ref().unwrap().sections[0].findings[0]
            .observation
            .origin,
        Origin::Physician
    ));
    p.annotations.clear();
    let removed = integration::apply(&manual, &dictionary, &session(p)).unwrap();
    assert!(removed.annotations.is_empty());
    assert_eq!(
        removed.protocol.as_ref().unwrap().sections[0]
            .findings
            .len(),
        1
    );
}
#[test]
fn negative_finding_conflict_and_foreign_asset_do_not_mutate_the_protocol() {
    let (mut detail, dictionary, asset) = fixture::fixture();
    let mut p = Photo::new(1000, 800);
    p.id = asset;
    p.add_callout(7, 0.2, 0.3);
    detail.protocol.as_mut().unwrap().sections[0]
        .findings
        .push(Finding {
            id: uuid::Uuid::new_v4(),
            term_code: "cervix.minor.thin_acetowhite_epithelium".into(),
            observation: Field {
                value: FeatureState::Absent,
                origin: Origin::Physician,
            },
            physician_confirmed: true,
        });
    let before = json!(detail);
    assert!(integration::apply(&detail, &dictionary, &session(p.clone())).is_err());
    assert_eq!(json!(detail), before);
    p.id = uuid::Uuid::new_v4();
    assert!(integration::apply(&detail, &dictionary, &session(p)).is_err());
}
#[test]
fn receipt_serialization_is_stable_inside_gpui_dependency_graph() {
    let value: serde_json::Value = serde_json::from_str(
        r#"{"scope":"/s","body":{"z":"я","a":1},"version_id":null,"entity_id":"x"}"#,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(m53_contracts::canonical_json::to_vec(&value).unwrap()).unwrap(),
        r#"{"body":{"a":1,"z":"я"},"entity_id":"x","scope":"/s","version_id":null}"#
    );
}
#[test]
fn approved_protocol_allows_only_view_preferences() {
    let (mut detail, dictionary, asset) = fixture::fixture();
    let mut p = Photo::new(1000, 800);
    p.id = asset;
    p.add_callout(7, 0.2, 0.3);
    detail = integration::apply(&detail, &dictionary, &session(p.clone())).unwrap();
    p.annotations = detail.annotations.clone();
    detail.study.status = "approved".into();
    p.hidden.push(p.annotations[0].id);
    assert!(integration::apply(&detail, &dictionary, &session(p.clone())).is_ok());
    p.annotations[0].x = 0.8;
    assert!(integration::apply(&detail, &dictionary, &session(p)).is_err());
}

#[test]
fn stale_form_edits_preserve_new_annotations_on_reopen_and_followup_input() {
    let (detail, dictionary, asset) = fixture::fixture();
    let base = json!(detail.protocol);
    let mut typed = base.clone();
    typed["conclusion"] = json!({"origin":{"kind":"physician"},"value":{"state":"assessed","value":"Первое заключение"}});
    let first = m53_gpui_editor::state::form(&detail, &base, &typed).unwrap();
    let mut photo = Photo::new(1000, 800);
    photo.id = asset;
    photo.add_callout(7, 0.3, 0.4);
    let annotated = integration::apply(&first, &dictionary, &session(photo)).unwrap();
    let mut second = typed.clone();
    second["conclusion"]["value"]["value"] = json!("Заключение дополнено");
    let merged = m53_gpui_editor::state::form(&annotated, &typed, &second).unwrap();
    let mut journal = json!({"native_protocol":typed});
    m53_gpui_editor::state::set_detail(&mut journal, &merged);
    let reopened: m53_contracts::workflow::StudyDetail =
        serde_json::from_value(journal["detail"].clone()).unwrap();
    assert_eq!(
        reopened.protocol.as_ref().unwrap().sections[0]
            .findings
            .len(),
        1
    );
    assert_eq!(
        reopened.annotations[0].finding_id,
        Some(reopened.protocol.as_ref().unwrap().sections[0].findings[0].id)
    );
    assert_eq!(journal["native_protocol"], json!(reopened.protocol));
    assert_eq!(
        journal["native_protocol"]["conclusion"]["value"]["value"],
        "Заключение дополнено"
    );
}

#[test]
fn form_rejects_broken_links_and_same_field_conflicts_without_mutation() {
    let (detail, dictionary, asset) = fixture::fixture();
    let mut photo = Photo::new(1000, 800);
    photo.id = asset;
    photo.add_callout(7, 0.3, 0.4);
    let annotated = integration::apply(&detail, &dictionary, &session(photo)).unwrap();
    let base = json!(annotated.protocol);
    let mut incoming = base.clone();
    incoming["sections"][0]["findings"] = json!([]);
    assert!(m53_gpui_editor::state::form(&annotated, &base, &incoming).is_err());
    assert_eq!(json!(annotated.protocol), base);
    assert!(
        m53_gpui_editor::state::merge(
            &json!({"text":"old"}),
            &json!({"text":"marker"}),
            &json!({"text":"form"})
        )
        .is_err()
    );
    let merged = m53_gpui_editor::state::merge(
        &json!({"rows":[]}),
        &json!({"rows":[{"id":"1","value":1}]}),
        &json!({"rows":[{"id":"2","value":2}]}),
    )
    .unwrap();
    assert_eq!(merged["rows"].as_array().unwrap().len(), 2);
}

#[test]
fn per_marker_properties_survive_delete_undo_and_do_not_leak_to_other_marker() {
    let (mut detail, dictionary, asset) = fixture::fixture();
    detail.protocol.as_mut().unwrap().schema_version = 2;
    detail.protocol.as_mut().unwrap().colpo = Some(Default::default());
    let mut photo = Photo::new(1000, 800);
    photo.id = asset;
    let a = photo.add_callout(8, 0.3, 0.3);
    let b = photo.add_callout(8, 0.7, 0.7);
    let mut staged = integration::apply(&detail, &dictionary, &session(photo.clone())).unwrap();
    let meta = staged.protocol.as_mut().unwrap().colpo.as_mut().unwrap();
    meta.marks.get_mut(&a).unwrap().properties.insert(
        "property.rapid".into(),
        m53_domain::colpo::Answer::Checked(true),
    );
    meta.marks.get_mut(&b).unwrap().properties.insert(
        "property.persistent".into(),
        m53_domain::colpo::Answer::Checked(true),
    );
    let before = photo.clone();
    photo.annotations.retain(|p| p.id != a);
    let deleted = integration::apply(&staged, &dictionary, &session(photo)).unwrap();
    let restored = integration::apply(&deleted, &dictionary, &session(before)).unwrap();
    let meta = restored.protocol.as_ref().unwrap().colpo.as_ref().unwrap();
    assert!(meta.marks[&a].properties.contains_key("property.rapid"));
    assert!(!meta.marks[&b].properties.contains_key("property.rapid"));
    assert!(
        meta.marks[&b]
            .properties
            .contains_key("property.persistent")
    );
    assert!(
        m53_contracts::workflow::validate_colpo_links(
            restored.protocol.as_ref().unwrap(),
            &restored.annotations
        )
        .is_ok()
    );
}

#[test]
fn schiller_replacement_is_per_image_and_undo_restores_all_markers() {
    let (mut detail, dictionary, asset) = fixture::fixture();
    detail.protocol.as_mut().unwrap().schema_version = 2;
    detail.protocol.as_mut().unwrap().colpo = Some(Default::default());
    let other = uuid::Uuid::new_v4();
    let mut other_asset = detail.assets[0].clone();
    other_asset.id = other;
    detail.assets.push(other_asset);
    let mut photo = Photo::new(1000, 800);
    photo.id = asset;
    photo.add_callout(6, 0.2, 0.2);
    photo.add_callout(6, 0.3, 0.3);
    let mut second = Photo::new(1000, 800);
    second.id = other;
    second.add_callout(6, 0.5, 0.5);
    let before = Session {
        photos: vec![photo.clone(), second.clone()],
        selected: 0,
    };
    let initial = integration::apply(&detail, &dictionary, &before).unwrap();
    photo.add_callout(11, 0.7, 0.7);
    let replaced = integration::apply(
        &initial,
        &dictionary,
        &Session {
            photos: vec![photo, second],
            selected: 0,
        },
    )
    .unwrap();
    let meta = replaced.protocol.as_ref().unwrap().colpo.as_ref().unwrap();
    assert_eq!(
        meta.schiller(asset),
        Some(m53_domain::colpo::SchillerResult::Negative)
    );
    assert_eq!(
        meta.schiller(other),
        Some(m53_domain::colpo::SchillerResult::Positive)
    );
    assert_eq!(
        replaced
            .annotations
            .iter()
            .filter(|a| a.asset_id == asset)
            .count(),
        1
    );
    let undone = integration::apply(&replaced, &dictionary, &before).unwrap();
    assert_eq!(
        undone
            .annotations
            .iter()
            .filter(|a| a.asset_id == asset)
            .count(),
        2
    );
    let mut invalid = undone.protocol.as_ref().unwrap().clone();
    invalid
        .colpo
        .as_mut()
        .unwrap()
        .marks
        .values_mut()
        .next()
        .unwrap()
        .catalog_id = "cervix.symbol.11".into();
    assert!(m53_contracts::workflow::validate_colpo_links(&invalid, &undone.annotations).is_err());
}

#[test]
fn v1_serialization_has_no_extension_and_v2_form_preserves_receipt_inputs() {
    let (detail, _, _) = fixture::fixture();
    let before = json!(detail.protocol);
    assert!(before.get("colpo").is_none());
    let roundtrip: Protocol = serde_json::from_value(before.clone()).unwrap();
    assert_eq!(
        m53_contracts::canonical_json::to_vec(&before).unwrap(),
        m53_contracts::canonical_json::to_vec(&json!(roundtrip)).unwrap()
    );
}

#[test]
fn retag_keeps_lesion_ownership_without_stealing_a_shared_section_finding() {
    let (mut detail, dictionary, asset) = fixture::fixture();
    detail.protocol.as_mut().unwrap().schema_version = 2;
    detail.protocol.as_mut().unwrap().colpo = Some(Default::default());
    let a = m53_client::defects::place(&mut detail, &dictionary, 7, asset, 0.3, 0.3).unwrap();
    let b = m53_client::defects::place(&mut detail, &dictionary, 8, asset, 0.7, 0.7).unwrap();
    let a_fid = detail
        .annotations
        .iter()
        .find(|v| v.id == a)
        .unwrap()
        .finding_id
        .unwrap();
    let b_fid = detail
        .annotations
        .iter()
        .find(|v| v.id == b)
        .unwrap()
        .finding_id
        .unwrap();
    let lesion = uuid::Uuid::new_v4();
    let section = &mut detail.protocol.as_mut().unwrap().sections[0];
    let index = section.findings.iter().position(|f| f.id == a_fid).unwrap();
    let finding = section.findings.remove(index);
    let field = m53_client::model::field;
    section.lesions.push(Lesion {
        id: lesion,
        location: Location::Cervix {
            zone_relation: field(Assessment::NotAssessed),
            clock_start: m53_client::model::field(Assessment::NotAssessed),
            clock_end: m53_client::model::field(Assessment::NotAssessed),
            quadrants: m53_client::model::field(Assessment::NotAssessed),
            affected_percent: m53_client::model::field(Assessment::NotAssessed),
        },
        findings: vec![finding],
    });
    detail
        .annotations
        .iter_mut()
        .find(|v| v.id == a)
        .unwrap()
        .lesion_id = Some(lesion);
    m53_client::defects::retag(&mut detail, &dictionary, a, 8).unwrap();
    let a = detail.annotations.iter().find(|v| v.id == a).unwrap();
    assert_eq!(a.lesion_id, Some(lesion));
    assert_ne!(a.finding_id, Some(b_fid));
    assert!(
        detail.protocol.as_ref().unwrap().sections[0]
            .findings
            .iter()
            .any(|f| f.id == b_fid)
    );
    assert!(
        m53_contracts::workflow::validate_colpo_links(
            detail.protocol.as_ref().unwrap(),
            &detail.annotations
        )
        .is_ok()
    );
}
