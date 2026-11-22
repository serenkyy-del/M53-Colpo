use m53_client::{defects::*, imaging, model::blank_protocol, report};
use m53_contracts::workflow::*;
use m53_domain::{ExaminationState, FeatureState, protocol::*, validation::*};
use uuid::Uuid;
fn fixture() -> (StudyDetail, Dictionary, Uuid) {
    let id = Uuid::new_v4();
    let asset = Uuid::new_v4();
    let dictionary = Dictionary {
        version_id: id,
        clinically_approved: false,
        terms: PRESETS
            .iter()
            .filter_map(|p| {
                p.code.map(|code| Term {
                    code: code.into(),
                    label: p.name.into(),
                    region: Region::Cervix,
                    kind: TermKind::Finding,
                })
            })
            .collect(),
    };
    (
        StudyDetail {
            mis_task_bound: false,
            study: Study {
                id,
                patient_id: id,
                status: "draft".into(),
                row_version: 1,
                study_instance_uid: "2.25.1".into(),
            },
            patient: Patient { card_number: None,
                sex: None,
                id,
                external_id: None,
                family_name: "Тест".into(),
                given_name: "Тест".into(),
                patronymic: None,
                birth_date: None,
            },
            revision_id: Some(id),
            protocol: Some(blank_protocol(id)),
            annotations: vec![],
            assets: vec![Asset {
                id: asset,
                study_id: id,
                kind: "original".into(),
                sha256: "a".repeat(64),
                byte_size: 1,
                mime_type: "image/png".into(),
            }],
        },
        dictionary,
        asset,
    )
}
#[test]
fn callout_is_linked_to_one_finding_and_direct_handles_are_independent() {
    let (mut d, dict, asset) = fixture();
    let id = place(&mut d, &dict, 9, asset, 0.7, 0.6).unwrap();
    assert_eq!(d.annotations[0].label, "Л");
    let fid = d.annotations[0].finding_id.unwrap();
    let section = &d.protocol.as_ref().unwrap().sections[0];
    assert_eq!(section.state.value, ExaminationState::Examined);
    assert_eq!(section.findings[0].id, fid);
    assert!(validate(d.protocol.as_ref().unwrap(), &dict, ValidationMode::Draft).is_empty());
    let a = &mut d.annotations[0];
    let original = (a.x, a.y, a.radius);
    edit(a, Handle::Tip, 0.9, 0.8, 2.0);
    assert_eq!((a.x, a.y, a.radius), original);
    edit(a, Handle::Body, 0.2, 0.3, 2.0);
    assert!(matches!(
        a.layer,
        AnnotationLayer::Callout {
            end_x: 0.9,
            end_y: 0.8,
            ..
        }
    ));
    edit(a, Handle::Radius, 0.25, 0.3, 2.0);
    assert!((a.radius - 0.1).abs() < 0.00001);
    assert_eq!((a.x, a.y), (0.2, 0.3));
    place(&mut d, &dict, 9, asset, 0.4, 0.4).unwrap();
    assert_eq!(d.protocol.as_ref().unwrap().sections[0].findings.len(), 1);
    assert!(d.annotations.iter().all(Annotation::valid_layer));
    let remaining_id = d.annotations[1].id;
    let mut shared = d.clone();
    remove(&mut shared, id);
    assert_eq!(
        shared.protocol.as_ref().unwrap().sections[0].findings.len(),
        1
    );
    assert!(
        matches!(shared.protocol.as_ref().unwrap().sections[0].findings[0].observation.origin,Origin::Annotation{annotation_id} if annotation_id==remaining_id)
    );

    let persisted = serde_json::to_vec(&d).unwrap();
    let restored: StudyDetail = serde_json::from_slice(&persisted).unwrap();
    assert_eq!(restored.annotations[0].id, id);
}
#[test]
fn retag_is_atomic_and_does_not_override_negative_or_manually_edited_findings() {
    let (mut d, dict, asset) = fixture();
    let id = place(&mut d, &dict, 9, asset, 0.7, 0.6).unwrap();
    retag(&mut d, &dict, id, 10).unwrap();
    assert_eq!(d.annotations[0].id, id);
    assert_eq!(d.annotations[0].label, "Э");
    assert_eq!(d.protocol.as_ref().unwrap().sections[0].findings.len(), 1);
    let f = &mut d.protocol.as_mut().unwrap().sections[0].findings[0];
    f.observation.origin = Origin::Physician;
    remove(&mut d, id);
    assert_eq!(d.protocol.as_ref().unwrap().sections[0].findings.len(), 1);
    d.protocol.as_mut().unwrap().sections[0].findings[0]
        .observation
        .value = FeatureState::Absent;
    let id = place(&mut d, &dict, 0, asset, 0.5, 0.5).unwrap();
    let before = serde_json::to_vec(&d).unwrap();
    assert!(retag(&mut d, &dict, id, 10).is_err());
    assert_eq!(serde_json::to_vec(&d).unwrap(), before);
    d.study.status = "approved".into();
    let before = serde_json::to_vec(&d).unwrap();
    remove(&mut d, id);
    assert!(place(&mut d, &dict, 0, asset, 0.5, 0.5).is_err());
    assert_eq!(serde_json::to_vec(&d).unwrap(), before);
}
#[test]
fn opaque_badge_renders_and_full_defect_names_are_exported_without_touching_original() {
    let (mut d, dict, asset) = fixture();
    place(&mut d, &dict, 9, asset, 0.75, 0.65).unwrap();
    place(&mut d, &dict, 0, asset, 0.3, 0.3).unwrap();
    let source = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        800,
        600,
        image::Rgb([14, 25, 36]),
    ));
    let bytes = imaging::png(&source).unwrap();
    let rendered = imaging::annotated(&source, &d.annotations).to_rgb8();
    let a = &d.annotations[0];
    assert_eq!(
        rendered
            .get_pixel(
                (a.x * 800.0) as u32,
                (a.y * 600.0 + a.radius * 600.0 * 0.5) as u32
            )
            .0,
        [255, 255, 255]
    );
    assert_eq!(imaging::png(&source).unwrap(), bytes);
    let result = report::render_with_terms(
        &d,
        &ReportLayout {
            title: "Тест".into(),
            photo_columns: 1, letterhead: None,
            blocks: vec![ReportBlock::Patient, ReportBlock::Photos],
        },
        &[report::Photo {
            id: asset,
            bytes,
            caption: "Снимок 1".into(),
        }],
        "Тест",
        &dict.terms,
    )
    .unwrap();
    let html = String::from_utf8(result.html).unwrap();
    assert!(html.contains("Л — Лейкоплакия"));
    assert!(html.contains("Р — Разрывы"));
    assert!(result.pdf.starts_with(b"%PDF-"));
}
