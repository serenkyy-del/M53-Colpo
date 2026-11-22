use m53_domain::{colpo::{Answer, Colpo, Mark}, protocol::{Assessment, Field, Origin, Protocol, ProtocolDiagnosis}, visit_template::VisitTemplate};
use std::collections::BTreeMap;
use uuid::Uuid;

fn fixture() -> Protocol {
    let value: serde_json::Value = serde_json::from_str(include_str!("../../../examples/protocol/draft.json")).unwrap();
    serde_json::from_value(value["protocol"].clone()).unwrap()
}
#[test]
fn portable_template_preserves_dates_and_images_and_replaces_manual_values() {
    let mut source = fixture();
    let source_row = Uuid::from_u128(123);
    source.diagnoses = Some(vec![ProtocolDiagnosis { id:source_row, diagnosis_id:Uuid::from_u128(456), code:"N87".into(), name:"Пример".into(), additional_text:String::new() }]);
    source.history = Field { value: Assessment::Assessed("Шаблон анамнеза".into()), origin: Origin::Physician };
    let asset = Uuid::from_u128(999);
    let mark = Uuid::from_u128(998);
    source.colpo = Some(Colpo { fields: BTreeMap::from([
        ("context.exam_date".into(), Answer::Text("2026-10-28".into())),
        ("context.lmp".into(), Answer::Text("2026-10-02".into())),
        ("simple.relief".into(), Answer::Choice("Ровный".into())),
    ]), marks:BTreeMap::from([(mark, Mark { asset_id:asset, catalog_id:"cervix.symbol.6".into(), properties:BTreeMap::new() })]), mark_history:BTreeMap::from([("previous".into(), Mark {asset_id: Uuid::from_u128(997), catalog_id:"cervix.symbol.6".into(), properties:BTreeMap::new() } )]) });
    let template = VisitTemplate::capture(&source).unwrap();
    let encoded = serde_json::to_string(&template).unwrap();
    for forbidden in ["context.exam_date", "context.lmp", "2026-10-28", &asset.to_string(), &mark.to_string(), &Uuid::from_u128(997).to_string(), &source_row.to_string()] {
        assert!(!encoded.contains(forbidden), "portable template leaked {forbidden}");
    }
    let mut target = source.clone();
    target.history = Field { value:Assessment::Assessed("Старый анамнез".into()), origin:Origin::Physician };
    target.colpo.as_mut().unwrap().fields.insert("context.lmp".into(), Answer::Text("2026-09-01".into()));
    let old_images = target.colpo.as_ref().unwrap().marks.clone();
    let old_history = target.colpo.as_ref().unwrap().mark_history.clone();
    template.apply(&mut target).unwrap();
    assert_eq!(target.history, source.history);
    assert_eq!(target.colpo.as_ref().unwrap().fields["context.lmp"], Answer::Text("2026-09-01".into()));
    assert_eq!(target.colpo.as_ref().unwrap().marks, old_images);
    assert_eq!(target.colpo.as_ref().unwrap().mark_history, old_history);
    assert_eq!(target.diagnoses.as_ref().unwrap()[0].diagnosis_id, Uuid::from_u128(456));
    assert_ne!(target.diagnoses.as_ref().unwrap()[0].id, source_row);
}

#[test]
fn invalid_or_image_derived_template_never_changes_target() {
    let source = fixture();
    let mut template = VisitTemplate::capture(&source).unwrap();
    template.colpo_fields.insert("context.lmp".into(), Answer::Text("2026-10-02".into()));
    let mut target = source.clone();
    assert!(template.apply(&mut target).is_err());
    assert_eq!(target, source);
    assert!(serde_json::from_str::<VisitTemplate>(r#"{"format_version":1,"marks":{}}"#).is_err());
}

#[test]
fn clearing_physician_fields_keeps_dates_image_data_and_annotation_origin() {
    let mut target = fixture();
    target.history = Field { value:Assessment::Assessed("Текст врача".into()), origin:Origin::Physician };
    target.conclusion = Field { value:Assessment::Assessed("Из снимка".into()), origin:Origin::Annotation { annotation_id:Uuid::from_u128(99) } };
    target.colpo = Some(Colpo { fields:BTreeMap::from([
        ("context.exam_date".into(), Answer::Text("2026-10-28".into())),
        ("context.lmp".into(), Answer::Text("2026-10-02".into())),
        ("simple.relief".into(), Answer::Choice("Ровный".into())),
    ]), marks:BTreeMap::from([(Uuid::from_u128(98), Mark { asset_id:Uuid::from_u128(97), catalog_id:"cervix.symbol.6".into(), properties:BTreeMap::new() })]), mark_history:BTreeMap::new() });
    let old_image = target.colpo.as_ref().unwrap().marks.clone();
    let derived = target.conclusion.clone();
    VisitTemplate::clear(&mut target);
    assert_eq!(target.history.value, Assessment::NotAssessed);
    assert_eq!(target.conclusion, derived);
    let colpo = target.colpo.as_ref().unwrap();
    assert_eq!(colpo.marks, old_image);
    assert_eq!(colpo.fields["context.lmp"], Answer::Text("2026-10-02".into()));
    assert_eq!(colpo.fields["context.exam_date"], Answer::Text("2026-10-28".into()));
    assert!(!colpo.fields.contains_key("simple.relief"));
    assert_eq!(target.diagnoses, Some(vec![]));
}

#[test]
fn annotation_derived_value_is_not_saved_or_overwritten() {
    let mut source = fixture();
    source.conclusion = Field { value: Assessment::Assessed("Данные снимка".into()), origin:Origin::Annotation { annotation_id:Uuid::from_u128(42) } };
    let template = VisitTemplate::capture(&source).unwrap();
    assert!(!serde_json::to_string(&template).unwrap().contains("Данные снимка"));
    let mut target = fixture();
    target.conclusion = Field { value:Assessment::Assessed("Ввод врача".into()), origin:Origin::Physician };
    template.apply(&mut target).unwrap();
    assert_eq!(target.conclusion.value, Assessment::Assessed("Ввод врача".into()));
}

#[test]
fn capture_rejects_invalid_doctor_input_instead_of_silently_dropping_it() {
    let mut source = fixture();
    let colpo = source.colpo.get_or_insert_with(Colpo::default);
    colpo.fields.insert("context.pregnancy".into(), Answer::Text("неверное значение".into()));
    assert!(VisitTemplate::capture(&source).is_err());
}
