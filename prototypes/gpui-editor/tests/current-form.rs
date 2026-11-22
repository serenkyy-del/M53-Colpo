#[path = "../src/verification.rs"]
mod fixture;
use m53_gpui_editor::{Photo, Session, integration, state};
use serde_json::json;

#[test]
fn existing_draft_opens_current_form_preserving_original_fields_and_mark_links() {
    let (detail, dictionary, asset) = fixture::fixture();
    let mut photo = Photo::new(1000, 800);
    photo.id = asset;
    photo.add_callout(0, 0.3, 0.4);
    let mut detail = integration::apply(
        &detail,
        &dictionary,
        &Session {
            photos: vec![photo],
            selected: 0,
        },
    )
    .unwrap();
    let protocol = detail.protocol.as_mut().unwrap();
    let old_text = m53_domain::protocol::Field {
        value: m53_domain::protocol::Assessment::Assessed("Существующее значение".into()),
        origin: m53_domain::protocol::Origin::Physician,
    };
    protocol.clinical_context = Some(m53_domain::protocol::ClinicalContext {
        cycle: old_text.clone(),
        pregnancy: old_text.clone(),
        menopause: old_text.clone(),
        method: old_text.clone(),
    });
    protocol.conclusion = old_text;
    let original = json!(detail);
    assert!(state::use_current_form(&mut detail));
    let mut converted = json!(detail);
    assert_eq!(converted["protocol"]["schema_version"], 2);
    assert_eq!(
        converted["protocol"]["colpo"]["marks"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    converted["protocol"]
        .as_object_mut()
        .unwrap()
        .remove("colpo");
    converted["protocol"]["schema_version"] = json!(1);
    // Opening the new form must leave every previous clinical value unchanged.
    assert_eq!(converted, original);
    let converted = json!(detail);
    assert!(!state::use_current_form(&mut detail));
    assert_eq!(json!(detail), converted);
}

#[test]
fn approved_study_is_never_migrated() {
    let (mut detail, _, _) = fixture::fixture();
    detail.study.status = "approved".into();
    let original = json!(detail);
    assert!(!state::use_current_form(&mut detail));
    assert_eq!(json!(detail), original);
}
