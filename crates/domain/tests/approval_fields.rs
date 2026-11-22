use m53_domain::protocol::*;
use m53_domain::validation::*;
use m53_domain::ExaminationState;
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

fn modern() -> (Protocol, Dictionary) {
    let (mut p,d)=fixture();
    p.schema_version=2;
    p.colpo=Some(Default::default());
    (p,d)
}
fn fill(p:&mut Protocol) {
    for id in m53_domain::approval_fields::REQUIRED_CERVIX_FIELDS.iter().chain(m53_domain::approval_fields::REQUIRED_SIMPLE_FIELDS) {
        let f=m53_domain::colpo::FIELDS.iter().find(|f|f.id==*id).unwrap();
        p.colpo.as_mut().unwrap().fields.insert(id.to_string(),m53_domain::colpo::Answer::Choice(f.choices[0].into()));
    }
    p.sections[0].state=field(ExaminationState::Examined);
    p.sections[0].adequacy=field(Assessment::Assessed(Adequacy::Adequate));
    p.cervical_assessment.transformation_zone=field(Assessment::Assessed(CervicalTransformationZone::Type1));
}
fn has_path(p:&Protocol,d:&Dictionary,path:&str)->bool {
    validate(p,d,ValidationMode::Approval).iter().any(|e|e.path==path)
}
#[test]
fn incomplete_modern_draft_saves_but_approval_names_each_manual_field_without_mutation() {
    let (p,d)=modern();let before=p.clone();
    assert!(validate(&p,&d,ValidationMode::Draft).is_empty());
    let errors=validate(&p,&d,ValidationMode::Approval);
    for id in m53_domain::approval_fields::REQUIRED_CERVIX_FIELDS.iter().chain(m53_domain::approval_fields::REQUIRED_SIMPLE_FIELDS) {
        assert!(errors.iter().any(|e|e.path==format!("/colpo/fields/{id}")&&e.code=="required_colpo_field"));
    }
    assert_eq!(p,before);
}
#[test]
fn valid_choices_clear_errors_unknown_wrong_kind_and_blank_answers_do_not() {
    let (mut p,d)=modern();fill(&mut p);
    assert!(validate(&p,&d,ValidationMode::Approval).is_empty());
    for answer in [m53_domain::colpo::Answer::Choice(" ".into()),m53_domain::colpo::Answer::Choice("unknown".into()),m53_domain::colpo::Answer::Text("Цилиндрическая".into()),m53_domain::colpo::Answer::Checked(true)] {
        p.colpo.as_mut().unwrap().fields.insert("cervix.shape".into(),answer);
        assert!(has_path(&p,&d,"/colpo/fields/cervix.shape"));
    }
}
#[test]
fn both_explicit_hidden_flags_exempt_cervix_and_preserve_data_while_false_does_not() {
    for flag in ["context.no_cervix","context.skip"] {
        let (mut p,d)=modern();
        p.colpo.as_mut().unwrap().fields.insert(flag.into(),m53_domain::colpo::Answer::Checked(true));
        let before=p.clone();
        assert!(validate(&p,&d,ValidationMode::Approval).is_empty());
        assert_eq!(p,before);
        p.colpo.as_mut().unwrap().fields.insert(flag.into(),m53_domain::colpo::Answer::Checked(false));
        assert!(has_path(&p,&d,"/colpo/fields/cervix.os"));
    }
}
#[test]
fn counts_are_independent_and_only_required_for_their_checked_glands() {
    let (mut p,d)=modern();fill(&mut p);
    for (flag,count) in [("simple.closed","simple.closed_count"),("simple.open","simple.open_count")] {
        assert!(!has_path(&p,&d,&format!("/colpo/fields/{count}")));
        p.colpo.as_mut().unwrap().fields.insert(flag.into(),m53_domain::colpo::Answer::Checked(true));
        assert!(has_path(&p,&d,&format!("/colpo/fields/{count}")));
        p.colpo.as_mut().unwrap().fields.insert(flag.into(),m53_domain::colpo::Answer::Checked(false));
        assert!(!has_path(&p,&d,&format!("/colpo/fields/{count}")));
    }
}
#[test]
fn abnormal_location_size_apply_to_manual_or_image_finding_and_disappear_when_removed() {
    for mark in [false,true] {
        let (mut p,d)=modern();fill(&mut p);
        assert!(!has_path(&p,&d,"/colpo/fields/extended.location"));
        if mark {
            p.colpo.as_mut().unwrap().marks.insert(id(99),m53_domain::colpo::Mark{asset_id:id(100),catalog_id:"cervix.symbol.7".into(),properties:Default::default()});
        } else {
            p.colpo.as_mut().unwrap().fields.insert("cervix.symbol.8".into(),m53_domain::colpo::Answer::Checked(true));
        }
        for key in ["extended.location","extended.size"] {assert!(has_path(&p,&d,&format!("/colpo/fields/{key}")));}
        p.colpo.as_mut().unwrap().marks.clear();
        p.colpo.as_mut().unwrap().fields.remove("cervix.symbol.8");
        assert!(!has_path(&p,&d,"/colpo/fields/extended.location"));
    }
}
#[test]
fn typed_zone_allows_explicit_not_applicable_and_adequacy_uses_region_not_index() {
    let (mut p,d)=modern();fill(&mut p);
    p.cervical_assessment.transformation_zone=field(Assessment::NotApplicable);
    assert!(validate(&p,&d,ValidationMode::Approval).is_empty());
    p.cervical_assessment.transformation_zone=field(Assessment::NotAssessed);
    assert!(has_path(&p,&d,"/cervical_assessment/transformation_zone"));
    p.sections.swap(0,2);
    p.sections[2].adequacy=missing();
    let errors=validate(&p,&d,ValidationMode::Approval);
    assert_eq!(errors.iter().filter(|e|e.code=="adequacy_required").count(),1);
    assert!(errors.iter().any(|e|e.path=="sections[2].adequacy"||e.path=="/sections/2/adequacy"));
}
#[test]
fn legacy_schema_is_unchanged_and_not_examined_is_not_a_missing_field_bypass() {
    let (legacy,d)=fixture();assert!(validate(&legacy,&d,ValidationMode::Approval).is_empty());
    let (mut p,d)=modern();fill(&mut p);
    p.sections[0].state=field(ExaminationState::NotExamined);
    p.sections[0].adequacy=missing();
    assert!(has_path(&p,&d,"/sections/0/adequacy"));
}

#[test]
fn examination_date_and_existing_skin_flags_survive_draft_validation() {
    let (mut p,d)=modern();
    let colpo=p.colpo.as_mut().unwrap();
    colpo.fields.insert("context.exam_date".into(),m53_domain::colpo::Answer::Text("2026-10-21".into()));
    for id in ["vulva.skin.red","vulva.skin.white","vulva.skin.dark"] {colpo.fields.insert(id.into(),m53_domain::colpo::Answer::Checked(true));}
    assert!(validate(&p,&d,ValidationMode::Draft).is_empty());
    p.colpo.as_mut().unwrap().fields.insert("context.exam_date".into(),m53_domain::colpo::Answer::Text("2026-02-30".into()));
    assert!(!validate(&p,&d,ValidationMode::Draft).is_empty());
}
#[test]
fn legacy_manual_acetowhite_flags_are_typed_and_do_not_open_arbitrary_keys() {
    let (mut p,d)=modern();fill(&mut p);
    for id in ["cervix.symbol.7","cervix.symbol.8"] {
        p.colpo.as_mut().unwrap().fields.insert(id.into(),m53_domain::colpo::Answer::Checked(true));
        assert!(validate(&p,&d,ValidationMode::Draft).is_empty());
        p.colpo.as_mut().unwrap().fields.insert(id.into(),m53_domain::colpo::Answer::Text("true".into()));
        assert!(!validate(&p,&d,ValidationMode::Draft).is_empty());
        p.colpo.as_mut().unwrap().fields.remove(id);
    }
    p.colpo.as_mut().unwrap().fields.insert("arbitrary.field".into(),m53_domain::colpo::Answer::Checked(true));
    assert!(!validate(&p,&d,ValidationMode::Draft).is_empty());
}
