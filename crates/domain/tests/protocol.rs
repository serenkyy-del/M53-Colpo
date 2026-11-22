use m53_domain::protocol::*;
use m53_domain::validation::*;
use m53_domain::{ExaminationState, FeatureState};
use uuid::Uuid;
fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn field<T>(value: T) -> Field<T> {
    Field {
        value,
        origin: Origin::Physician,
    }
}
fn missing<T>() -> Field<Assessment<T>> {
    field(Assessment::NotAssessed)
}
fn fixture() -> (Protocol, Dictionary) {
    let dictionary = Dictionary {
        version_id: id(1),
        clinically_approved: true,
        terms: vec![
            Term {
                label: String::new(),
                code: "test.cervix.feature".into(),
                region: Region::Cervix,
                kind: TermKind::Finding,
            },
            Term {
                label: String::new(),
                code: "test.cervix.probe".into(),
                region: Region::Cervix,
                kind: TermKind::Test,
            },
        ],
    };
    let protocol = Protocol {
        diagnosis_note: None,
        diagnoses: None,
        colpo: None,
        clinical_context: None,
        biopsies: vec![],
        schema_version: SCHEMA_VERSION,
        terminology_version_id: id(1),
        indications: missing(),
        history: missing(),
        sections: [Region::Cervix, Region::Vagina, Region::Vulva, Region::Anus]
            .into_iter()
            .map(|region| RegionSection {
                region,
                state: field(ExaminationState::NotExamined),
                adequacy: missing(),
                findings: vec![],
                lesions: vec![],
            })
            .collect(),
        cervical_assessment: CervicalAssessment {
            junction_visibility: missing(),
            transformation_zone: missing(),
        },
        tests: vec![],
        excision: ExcisionSection {
            state: field(ExaminationState::NotApplicable),
            specimens: vec![],
        },
        conclusion: field(Assessment::Assessed(
            "Synthetic physician conclusion".into(),
        )),
        recommendations: missing(),
    };
    (protocol, dictionary)
}
fn examine(protocol: &mut Protocol) {
    protocol.sections[0].state = field(ExaminationState::Examined);
    protocol.sections[0].adequacy = field(Assessment::Assessed(Adequacy::Adequate));
}
fn lesion(n: u128) -> Lesion {
    Lesion {
        id: id(n),
        location: Location::Cervix {
            zone_relation: missing(),
            clock_start: field(Assessment::Assessed(12)),
            clock_end: field(Assessment::Assessed(1)),
            quadrants: field(Assessment::Assessed(1)),
            affected_percent: field(Assessment::Assessed(10.0)),
        },
        findings: vec![],
    }
}
fn has(errors: &[ValidationError], code: &str) -> bool {
    errors.iter().any(|e| e.code == code)
}

fn diagnosis_rows() -> serde_json::Value {
    serde_json::json!([
        {"id":id(800),"diagnosis_id":id(1800),"code":"N87.1","name":"Синтетический диагноз Б","additional_text":"Первый в отчёте"},
        {"id":id(801),"diagnosis_id":id(1801),"code":"N87.0","name":"Синтетический диагноз А","additional_text":"Дополнение"}
    ])
}

#[test]
fn common_diagnosis_note_is_optional_independent_multiline_and_bounded() {
    let (p,d)=fixture();
    let legacy=serde_json::to_value(&p).unwrap();
    assert!(legacy.get("diagnosis_note").is_none());
    for text in ["", "Общее дополнение\nВторая строка", "я".repeat(8192).as_str()] {
        let mut raw=legacy.clone();
        raw["diagnosis_note"]=serde_json::json!(field(Assessment::Assessed(text.to_string())));
        let parsed:Protocol=serde_json::from_value(raw.clone()).expect("common diagnosis note without ICD rows must roundtrip");
        assert_eq!(serde_json::to_value(&parsed).unwrap(),raw);
        for mode in [ValidationMode::Draft,ValidationMode::Approval] {
            assert!(validate(&parsed,&d,mode).is_empty());
        }
    }
    let mut raw=legacy.clone();
    raw["diagnosis_note"]=serde_json::json!(field(Assessment::Assessed("я".repeat(8193))));
    let parsed:Protocol=serde_json::from_value(raw).unwrap();
    for mode in [ValidationMode::Draft,ValidationMode::Approval] {
        assert!(validate(&parsed,&d,mode).iter().any(|e|e.path=="diagnosis_note" && e.code=="text_too_long"));
    }
    let mut cleared=legacy;
    cleared["diagnosis_note"]=serde_json::json!(empty_history());
    let parsed:Protocol=serde_json::from_value(cleared.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(),cleared,"explicit clear must not become omission");
}

#[test]
fn diagnoses_legacy_omission_and_explicit_empty_remain_distinct_without_becoming_required() {
    let (p,d)=fixture();
    let legacy=serde_json::to_value(&p).unwrap();
    assert!(legacy.get("diagnoses").is_none());
    let parsed:Protocol=serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(),legacy);
    assert!(validate(&p,&d,ValidationMode::Approval).is_empty());
    let mut cleared=legacy.clone();cleared["diagnoses"]=serde_json::json!([]);
    let parsed:Protocol=serde_json::from_value(cleared.clone()).expect("explicit clear must be understood");
    assert_eq!(serde_json::to_value(parsed).unwrap(),cleared,"explicit [] must reach the server to clear the last diagnosis");
}

#[test]
fn diagnoses_keep_selection_snapshots_additional_text_and_array_order() {
    let (p,d)=fixture();let mut raw=serde_json::to_value(&p).unwrap();
    raw["diagnoses"]=diagnosis_rows();
    let parsed:Protocol=serde_json::from_value(raw.clone()).expect("ordered optional diagnoses must be supported");
    assert_eq!(serde_json::to_value(&parsed).unwrap(),raw);
    assert!(validate(&parsed,&d,ValidationMode::Draft).is_empty());
    assert!(validate(&parsed,&d,ValidationMode::Approval).is_empty());
    let mut reordered=raw.clone();reordered["diagnoses"].as_array_mut().unwrap().swap(0,1);
    let next:Protocol=serde_json::from_value(reordered.clone()).unwrap();
    assert_ne!(parsed,next,"order is clinical document content");
    assert_eq!(serde_json::to_value(next).unwrap(),reordered);
}

#[test]
fn diagnoses_validate_bounds_and_stable_row_identity_without_online_catalog_lookup() {
    let (p,d)=fixture();let mut base=serde_json::to_value(p).unwrap();base["diagnoses"]=diagnosis_rows();
    for (key,value,code) in [
        ("id",serde_json::json!(Uuid::nil()),"nil_id"),
        ("diagnosis_id",serde_json::json!(Uuid::nil()),"nil_id"),
        ("code",serde_json::json!(" "),"empty_value"),
        ("code",serde_json::json!("x".repeat(33)),"text_too_long"),
        ("name",serde_json::json!(""),"empty_value"),
        ("name",serde_json::json!("я".repeat(2049)),"text_too_long"),
        ("additional_text",serde_json::json!("я".repeat(8193)),"text_too_long"),
    ] {
        let mut raw=base.clone();raw["diagnoses"][0][key]=value;
        let parsed:Protocol=serde_json::from_value(raw).unwrap();
        for mode in [ValidationMode::Draft,ValidationMode::Approval] {
            assert!(validate(&parsed,&d,mode).iter().any(|e|e.path==format!("diagnoses[0].{key}")&&e.code==code),"{key}/{code}");
        }
    }
    let mut duplicate=base.clone();duplicate["diagnoses"][1]["id"]=duplicate["diagnoses"][0]["id"].clone();
    assert!(has(&validate(&serde_json::from_value(duplicate).unwrap(),&d,ValidationMode::Draft),"duplicate_id"));
    let mut many=base;many["diagnoses"]=serde_json::Value::Array((0..101).map(|i|{let mut row=diagnosis_rows()[0].clone();row["id"]=serde_json::json!(id(800+i));row}).collect());
    assert!(has(&validate(&serde_json::from_value(many).unwrap(),&d,ValidationMode::Draft),"too_many_diagnoses"));
}
#[test]
fn roundtrip_preserves_missing_absent_and_not_applicable() {
    for state in [
        FeatureState::Absent,
        FeatureState::NotAssessed,
        FeatureState::NotApplicable,
        FeatureState::Present,
    ] {
        let encoded = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<FeatureState>(&encoded).unwrap(),
            state
        );
    }
    let (p, _) = fixture();
    assert_eq!(
        serde_json::from_str::<Protocol>(&serde_json::to_string(&p).unwrap()).unwrap(),
        p
    );
    assert!(serde_json::from_str::<Assessment<String>>("null").is_err());
}
#[test]
fn inadequacy_is_independent_of_junction_visibility() {
    let (mut p, d) = fixture();
    examine(&mut p);
    p.sections[0].adequacy = field(Assessment::Assessed(Adequacy::Inadequate {
        reason: "Synthetic obstruction".into(),
    }));
    p.cervical_assessment.junction_visibility =
        field(Assessment::Assessed(JunctionVisibility::FullyVisible));
    assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
    p.sections[0].adequacy = field(Assessment::Assessed(Adequacy::Inadequate {
        reason: " ".into(),
    }));
    assert!(has(
        &validate(&p, &d, ValidationMode::Draft),
        "inadequacy_reason_required"
    ));
}
#[test]
fn findings_do_not_require_a_photo_but_annotation_proposals_need_confirmation() {
    let (mut p, d) = fixture();
    examine(&mut p);
    p.sections[0].findings.push(Finding {
        id: id(3),
        term_code: "test.cervix.feature".into(),
        observation: field(FeatureState::Present),
        physician_confirmed: false,
    });
    assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
    p.sections[0].findings[0].observation.origin = Origin::Annotation {
        annotation_id: id(9),
    };
    assert!(validate(&p, &d, ValidationMode::Draft).is_empty());
    assert!(has(
        &validate(&p, &d, ValidationMode::Approval),
        "annotation_requires_physician_confirmation"
    ));
    p.sections[0].findings[0].physician_confirmed = true;
    assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
}
#[test]
fn independent_lesions_and_region_specific_terms_are_checked() {
    let (mut p, d) = fixture();
    examine(&mut p);
    p.sections[0].lesions = vec![lesion(10), lesion(11)];
    assert!(validate(&p, &d, ValidationMode::Draft).is_empty());
    p.sections[0].lesions[1].id = id(10);
    assert!(has(
        &validate(&p, &d, ValidationMode::Draft),
        "duplicate_id"
    ));
    p.sections[1].lesions.push(lesion(12));
    let errors = validate(&p, &d, ValidationMode::Draft);
    assert!(has(&errors, "region_mismatch"));
    assert!(has(&errors, "unexamined_section_has_observations"));
}
#[test]
fn rejects_nonfinite_and_out_of_range_measurements() {
    for percent in [-1.0, 101.0, f64::NAN, f64::INFINITY] {
        let (mut p, d) = fixture();
        examine(&mut p);
        let mut l = lesion(10);
        if let Location::Cervix {
            affected_percent, ..
        } = &mut l.location
        {
            *affected_percent = field(Assessment::Assessed(percent));
        }
        p.sections[0].lesions.push(l);
        assert!(has(
            &validate(&p, &d, ValidationMode::Draft),
            "out_of_range"
        ));
    }
}
#[test]
fn probe_result_does_not_imply_application() {
    let (mut p, d) = fixture();
    examine(&mut p);
    p.tests.push(ProcedureTest {
        region: Region::Cervix,
        code: "test.cervix.probe".into(),
        application: field(TestApplication::NotPerformed),
        result: field(Assessment::Assessed("Synthetic result".into())),
    });
    assert!(has(
        &validate(&p, &d, ValidationMode::Draft),
        "result_without_performed_test"
    ));
    p.tests[0].application = field(TestApplication::Performed);
    p.tests[0].result = missing();
    assert!(validate(&p, &d, ValidationMode::Draft).is_empty());
}
#[test]
fn dictionary_must_match_region_version_and_approval() {
    let (mut p, mut d) = fixture();
    examine(&mut p);
    p.sections[0].findings.push(Finding {
        id: id(3),
        term_code: "unreviewed".into(),
        observation: field(FeatureState::NotAssessed),
        physician_confirmed: false,
    });
    assert!(has(
        &validate(&p, &d, ValidationMode::Draft),
        "unknown_or_inapplicable_term"
    ));
    p.sections[0].findings.clear();
    d.clinically_approved = false;
    assert!(validate(&p, &d, ValidationMode::Draft).is_empty());
    assert!(has(
        &validate(&p, &d, ValidationMode::Approval),
        "dictionary_not_approved"
    ));
    p.terminology_version_id = id(2);
    assert!(has(
        &validate(&p, &d, ValidationMode::Draft),
        "dictionary_version_mismatch"
    ));
}
#[test]
fn requires_explicit_regions_and_rejects_unknown_schema_fields() {
    let (mut p, d) = fixture();
    p.sections.pop();
    assert!(has(
        &validate(&p, &d, ValidationMode::Draft),
        "each_region_required_once"
    ));
    let mut json = serde_json::to_value(&p).unwrap();
    json["diagnosis_inferred"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Protocol>(json).is_err());
    assert!(
        serde_json::from_value::<Location>(
            serde_json::json!({"region":"vagina","transformation_zone":"type1"})
        )
        .is_err()
    );
}
#[test]
fn specimen_dimensions_are_explicit_and_circumference_is_optional() {
    let (mut p, d) = fixture();
    p.excision.state = field(ExaminationState::Examined);
    p.excision.specimens.push(ExcisionSpecimen {
        id: id(15),
        excision_type: field(Assessment::Assessed(ExcisionType::Type2)),
        length_mm: field(Assessment::Assessed(5.0)),
        thickness_mm: field(Assessment::Assessed(2.0)),
        circumference_mm: missing(),
    });
    assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
    p.excision.specimens[0].length_mm = missing();
    assert!(has(
        &validate(&p, &d, ValidationMode::Approval),
        "specimen_dimensions_and_type_required"
    ));
}

#[test]
fn biopsy_must_belong_to_examined_region_and_its_lesion() {
    let (mut p, d) = fixture();
    examine(&mut p);
    p.sections[0].lesions.push(lesion(50));
    p.biopsies.push(BiopsySpecimen {
        id: id(51),
        region: Region::Cervix,
        lesion_id: Some(id(50)),
        location: missing(),
        material: missing(),
        sent_for_histology: field(FeatureState::NotAssessed),
    });
    assert!(validate(&p, &d, ValidationMode::Draft).is_empty());
    p.biopsies[0].region = Region::Vagina;
    let errors = validate(&p, &d, ValidationMode::Draft);
    assert!(has(&errors, "biopsy_in_unexamined_region"));
    assert!(has(&errors, "biopsy_lesion_region_mismatch"));
}

fn schema2_approval_fixture() -> (Protocol, Dictionary) {
    use m53_domain::colpo::{Answer, Colpo, FIELDS};
    let (mut p, d) = fixture();
    p.schema_version = 2;
    p.colpo = Some(Colpo::default());
    examine(&mut p);
    p.cervical_assessment.transformation_zone =
        field(Assessment::Assessed(CervicalTransformationZone::Type1));
    for id in m53_domain::approval_fields::REQUIRED_SIMPLE_FIELDS {
        let spec = FIELDS.iter().find(|f| f.id == *id).unwrap();
        p.colpo
            .as_mut()
            .unwrap()
            .fields
            .insert((*id).into(), Answer::Choice(spec.choices[0].into()));
    }
    (p, d)
}
fn complete_manual_cervix(p: &mut Protocol) {
    use m53_domain::colpo::{Answer, FIELDS};
    for id in m53_domain::approval_fields::REQUIRED_CERVIX_FIELDS {
        let spec = FIELDS.iter().find(|f| f.id == *id).unwrap();
        p.colpo
            .as_mut()
            .unwrap()
            .fields
            .insert((*id).into(), Answer::Choice(spec.choices[0].into()));
    }
}
#[test]
fn schema2_manual_cervix_fields_are_approval_only_and_preserve_document() {
    let (mut p, d) = schema2_approval_fixture();
    let before = p.clone();
    assert!(validate(&p, &d, ValidationMode::Draft).is_empty());
    let errors = validate(&p, &d, ValidationMode::Approval);
    assert_eq!(errors.len(), 4);
    for id in m53_domain::approval_fields::REQUIRED_CERVIX_FIELDS {
        assert!(
            errors.iter().any(
                |e| e.path == format!("/colpo/fields/{id}") && e.code == "required_colpo_field"
            )
        );
    }
    assert_eq!(
        p, before,
        "Validation must never infer fields, examination states or pathology"
    );
    complete_manual_cervix(&mut p);
    assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
    assert!(p.colpo.as_ref().unwrap().marks.is_empty());
    for code in [
        "extended.other",
        "extended.clock",
        "rectovaginal.description",
    ] {
        assert!(!p.colpo.as_ref().unwrap().fields.contains_key(code));
    }
}
#[test]
fn schema2_cervix_exemptions_are_explicit_boolean_flags() {
    use m53_domain::colpo::{Answer, Colpo};
    for flag in ["context.no_cervix", "context.skip"] {
        let (mut p, d) = fixture();
        p.schema_version = 2;
        p.colpo = Some(Colpo::default());
        p.colpo
            .as_mut()
            .unwrap()
            .fields
            .insert(flag.into(), Answer::Checked(true));
        let before = p.clone();
        assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
        assert_eq!(p, before);
        p.colpo
            .as_mut()
            .unwrap()
            .fields
            .insert(flag.into(), Answer::Checked(false));
        assert_eq!(
            validate(&p, &d, ValidationMode::Approval)
                .iter()
                .filter(|e| matches!(
                    e.code.as_str(),
                    "required_colpo_field" | "adequacy_required" | "transformation_zone_required"
                ))
                .count(),
            9
        );
    }
}
#[test]
fn schema2_required_cervix_fields_reject_empty_unknown_and_wrong_answer_kind() {
    use m53_domain::{approval_fields, colpo::Answer};
    for answer in [
        Answer::Choice(String::new()),
        Answer::Choice("   ".into()),
        Answer::Choice("synthetic invalid option".into()),
        Answer::Text("Цилиндрическая".into()),
        Answer::Checked(true),
    ] {
        let (mut p, d) = schema2_approval_fixture();
        p.colpo
            .as_mut()
            .unwrap()
            .fields
            .insert("cervix.shape".into(), answer);
        let errors = validate(&p, &d, ValidationMode::Approval);
        assert!(errors.iter().any(|e|e.path=="/colpo/fields/cervix.shape" && e.code=="required_colpo_field"));
        assert_eq!(approval_fields::validate(&p).len(), 4);
    }
    let (p, d) = fixture();
    assert!(
        validate(&p, &d, ValidationMode::Approval).is_empty(),
        "Schema1 approval is unchanged"
    );
}
#[test]
fn schema2_simple_fields_and_typed_assessments_are_explicit_and_independent() {
    let (mut p, d) = schema2_approval_fixture();
    complete_manual_cervix(&mut p);
    for id in m53_domain::approval_fields::REQUIRED_SIMPLE_FIELDS {
        let mut missing = p.clone();
        missing.colpo.as_mut().unwrap().fields.remove(*id);
        assert!(validate(&missing, &d, ValidationMode::Draft).is_empty());
        assert!(
            validate(&missing, &d, ValidationMode::Approval)
                .iter()
                .any(|e| e.path == format!("/colpo/fields/{id}"))
        );
    }
    p.sections[0].adequacy = missing();
    p.cervical_assessment.transformation_zone = missing();
    let errors = m53_domain::approval_fields::validate(&p);
    assert!(errors.iter().any(|e| e.path == "/sections/0/adequacy"));
    assert!(
        errors
            .iter()
            .any(|e| e.path == "/cervical_assessment/transformation_zone")
    );
    // Semantic region lookup, not a hardcoded sections[0] convention.
    p.cervical_assessment.transformation_zone = field(Assessment::NotApplicable);
    assert!(
        !m53_domain::approval_fields::validate(&p)
            .iter()
            .any(|e| e.code == "transformation_zone_required")
    );
    p.sections.swap(0, 2);
    assert!(
        m53_domain::approval_fields::validate(&p)
            .iter()
            .any(|e| e.path == "/sections/2/adequacy")
    );
}
#[test]
fn schema2_gland_counts_are_required_separately_only_after_explicit_flags() {
    use m53_domain::colpo::Answer;
    let (mut p, d) = schema2_approval_fixture();
    complete_manual_cervix(&mut p);
    assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
    for flag in ["simple.closed", "simple.open"] {
        p.colpo
            .as_mut()
            .unwrap()
            .fields
            .insert(flag.into(), Answer::Checked(true));
    }
    let errors = m53_domain::approval_fields::validate(&p);
    assert_eq!(errors.len(), 2);
    p.colpo.as_mut().unwrap().fields.insert(
        "simple.closed_count".into(),
        Answer::Choice("Единичные".into()),
    );
    let errors = m53_domain::approval_fields::validate(&p);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].path, "/colpo/fields/simple.open_count");
    p.colpo
        .as_mut()
        .unwrap()
        .fields
        .insert("simple.open".into(), Answer::Checked(false));
    assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
}
#[test]
fn schema2_lesion_general_fields_require_explicit_abnormal_cervix_mark() {
    use m53_domain::colpo::Mark;
    let (mut p, _) = schema2_approval_fixture();
    complete_manual_cervix(&mut p);
    for catalog in [
        "cervix.symbol.2",
        "cervix.symbol.6",
        "cervix.symbol.11",
        "vulva.type.spot",
    ] {
        p.colpo.as_mut().unwrap().marks.insert(
            id(100),
            Mark {
                asset_id: id(200),
                catalog_id: catalog.into(),
                properties: Default::default(),
            },
        );
        assert!(m53_domain::approval_fields::validate(&p).is_empty());
    }
    p.colpo.as_mut().unwrap().marks.clear();
    for catalog in m53_domain::approval_fields::ANOMALOUS_CERVIX_PRESETS {
        p.colpo.as_mut().unwrap().marks.insert(
            id(101),
            Mark {
                asset_id: id(201),
                catalog_id: (*catalog).into(),
                properties: Default::default(),
            },
        );
        let errors = m53_domain::approval_fields::validate(&p);
        assert_eq!(errors.len(), 2);
        assert!(
            errors
                .iter()
                .any(|e| e.path == "/colpo/fields/extended.location")
        );
        assert!(
            errors
                .iter()
                .any(|e| e.path == "/colpo/fields/extended.size")
        );
    }
    p.colpo.as_mut().unwrap().marks.clear();
    p.colpo.as_mut().unwrap().fields.insert(
        "cervix.symbol.7".into(),
        m53_domain::colpo::Answer::Checked(true),
    );
    assert_eq!(m53_domain::approval_fields::validate(&p).len(), 2);
    assert!(
        validate(&p, &fixture().1, ValidationMode::Draft).is_empty(),
        "Explicit manual ABE checkbox is accepted structurally"
    );
    let before = p.clone();
    let _ = m53_domain::approval_fields::validate(&p);
    assert_eq!(p, before);
}

#[test]
fn windows_colpo_catalog_dates_skin_and_hidden_context_roundtrip() {
    use m53_domain::colpo::{Answer, Colpo};
    let (mut p, d) = fixture();
    p.schema_version = 2;
    let mut c = Colpo::default();
    c.fields.insert(
        "context.exam_date".into(),
        Answer::Text("2024-03-01".into()),
    );
    c.fields
        .insert("context.lmp".into(), Answer::Text("2024-02-28".into()));
    for code in ["vulva.skin.red", "vulva.skin.white", "vulva.skin.dark"] {
        c.fields.insert(code.into(), Answer::Checked(true));
    }
    c.fields
        .insert("context.no_cervix".into(), Answer::Checked(true));
    p.colpo = Some(c);
    let before = p.clone();
    assert!(validate(&p, &d, ValidationMode::Draft).is_empty());
    assert!(validate(&p, &d, ValidationMode::Approval).is_empty());
    assert_eq!(p.colpo.as_ref().unwrap().cycle_day(), Some(3));
    let encoded = serde_json::to_vec(&p).unwrap();
    assert_eq!(
        serde_json::from_slice::<Protocol>(&encoded).unwrap(),
        before
    );
    p.colpo
        .as_mut()
        .unwrap()
        .fields
        .insert("context.no_cervix".into(), Answer::Checked(false));
    assert!(!m53_domain::approval_fields::validate(&p).is_empty());
    assert_eq!(
        p.colpo.as_ref().unwrap().fields.get("vulva.skin.red"),
        Some(&Answer::Checked(true))
    );
    for date in [
        "2023-02-29",
        "2024-02-30",
        "0000-01-01",
        "2024-13-01",
        "２０２４-01-01",
        "2024-+1-01",
        "2024-1-01",
    ] {
        p.colpo
            .as_mut()
            .unwrap()
            .fields
            .insert("context.exam_date".into(), Answer::Text(date.into()));
        assert!(
            !validate(&p, &d, ValidationMode::Draft).is_empty(),
            "Invalid Gregorian date accepted: {date}"
        );
    }
    p.colpo.as_mut().unwrap().fields.insert(
        "context.exam_date".into(),
        Answer::Text("2024-02-27".into()),
    );
    assert_eq!(p.colpo.as_ref().unwrap().cycle_day(), None);
    p.colpo.as_mut().unwrap().fields.insert(
        "context.exam_date".into(),
        Answer::Text("2024-03-01".into()),
    );
    for flag in ["context.pregnancy", "context.menopause"] {
        p.colpo
            .as_mut()
            .unwrap()
            .fields
            .insert(flag.into(), Answer::Checked(true));
        assert_eq!(p.colpo.as_ref().unwrap().cycle_day(), None);
    }
}

#[test]
fn optional_anamnesis_is_compatible_and_bounded_by_utf8_bytes() {
    let (mut protocol, dictionary) = fixture();
    for value in [Assessment::NotAssessed, Assessment::NotApplicable,
        Assessment::Assessed(String::new()), Assessment::Assessed(" \n\t".into()),
        Assessment::Assessed("я".repeat(8192)), Assessment::Assessed("<script>\nЭмодзи 🩺".into())] {
        protocol.history = field(value.clone());
        for mode in [ValidationMode::Draft, ValidationMode::Approval] {
            assert!(validate(&protocol,&dictionary,mode).iter().all(|e| e.path!="history"));
        }
        let encoded=serde_json::to_value(&protocol).unwrap();
        let decoded:Protocol=serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.history.value,value);
    }
    protocol.history=field(Assessment::Assessed("я".repeat(8193)));
    assert!(validate(&protocol,&dictionary,ValidationMode::Draft).iter().any(|e|e.path=="history"&&e.code=="text_too_long"));
    let mut old=serde_json::to_value(&protocol).unwrap();
    old.as_object_mut().unwrap().remove("history");
    let mut decoded:Protocol=serde_json::from_value(old).unwrap();
    assert_eq!(decoded.history,empty_history());
    decoded.history=field(Assessment::Assessed("Existing text\nНе стирать".into()));
    let mut old_client:Protocol=serde_json::from_slice(&serde_json::to_vec(&decoded).unwrap()).unwrap();
    old_client.recommendations=field(Assessment::Assessed("Another field".into()));
    assert_eq!(old_client.history,decoded.history);
}
