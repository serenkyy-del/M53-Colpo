use m53_client::{model, report};
use m53_contracts::workflow::*;
use m53_domain::protocol::{Assessment, ProtocolDiagnosis};
use uuid::Uuid;

fn fixture() -> (StudyDetail, ReportLayout) {
    let id = Uuid::from_u128(1);
    let mut protocol = model::blank_protocol(id);
    protocol.colpo = Some(Default::default());
    protocol.conclusion.value = Assessment::Assessed("PHYSICIAN_CONCLUSION".into());
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
            patient: Patient {
                id,
                external_id: None,
                family_name: "Synthetic".into(),
                given_name: "Test".into(),
                patronymic: None,
                birth_date: None,
                sex: None,
                card_number: None,
            },
            revision_id: Some(id),
            protocol: Some(protocol),
            assets: vec![],
            annotations: vec![],
        },
        ReportLayout {
            title: "Synthetic report".into(),
            blocks: vec![ReportBlock::Conclusion],
            photo_columns: 1,
            letterhead: None,
        },
    )
}

fn row(n: u128, code: &str, name: &str, extra: &str) -> ProtocolDiagnosis {
    ProtocolDiagnosis {
        id: Uuid::from_u128(n),
        diagnosis_id: Uuid::from_u128(n + 10),
        code: code.into(),
        name: name.into(),
        additional_text: extra.into(),
    }
}

#[test]
fn report_signature_has_only_right_aligned_physician() {
    let (mut detail, mut layout) = fixture();
    layout.blocks.push(ReportBlock::Signature);
    for structured in [true, false] {
        if !structured { detail.protocol.as_mut().unwrap().colpo=None; }
        let output=html(&detail,&layout);
        let native=report::native_lines(&detail,&layout,"Synthetic physician",&[]).unwrap().join("\n");
        for text in [&output,&native] {
            assert!(!text.contains("Ревизия:"));
            assert!(!text.contains("Словарь:"));
            assert_eq!(text.matches("Врач: Synthetic physician").count(),1);
        }
        assert!(output.contains("class='physician-signature'"));
        assert!(output.contains("text-align:right"));
    }
}
#[test]
fn selected_revision_report_keeps_its_patient_and_performer_when_another_doctor_opens_it() {
    let (mut detail,mut layout)=fixture();detail.study.status="approved".into();detail.patient.family_name="CURRENT_PATIENT".into();
    layout.blocks=vec![ReportBlock::Patient,ReportBlock::Conclusion,ReportBlock::Signature];
    let mut context=serde_json::json!({"study_id":detail.study.id,"revision_id":detail.revision_id,"status":"approved","readonly":true,"performer":{"display_name":"PERFORMER_A"},"editor":{"display_name":"EDITOR_B"},"approver":{"display_name":"APPROVER_C"},"viewer":"VIEWER_D","patient":detail.patient});
    context["patient"]["family_name"]=serde_json::json!("FROZEN_PATIENT");
    let (bound,performer)=m53_client::report_identity::bind(&detail,&context).unwrap();
    let output=String::from_utf8(report::preview_with_terms(&bound,&layout,&[],&performer,&[]).unwrap().html).unwrap();
    let native=report::native_lines(&bound,&layout,&performer,&[]).unwrap().join("\n");
    for text in [&output,&native]{assert!(text.contains("PERFORMER_A"));assert!(text.contains("FROZEN_PATIENT"));for wrong in ["CURRENT_PATIENT","EDITOR_B","APPROVER_C","VIEWER_D"]{assert!(!text.contains(wrong));}}
    assert_eq!(detail.patient.family_name,"CURRENT_PATIENT","создание исторического отчёта не меняет текущую карточку");
}

fn html(detail: &StudyDetail, layout: &ReportLayout) -> String {
    String::from_utf8(
        report::preview_with_terms(detail, layout, &[], "Synthetic physician", &[])
            .unwrap()
            .html,
    )
    .unwrap()
}

#[test]
fn approved_terminology_reports_have_no_test_version_notice() {
    let (mut detail, layout) = fixture();
    for structured in [true, false] {
        if !structured { detail.protocol.as_mut().unwrap().colpo = None; }
        let output = html(&detail, &layout);
        let native = report::native_lines(&detail, &layout, "Synthetic physician", &[]).unwrap().join("\n");
        for text in [output, native] {
            assert!(!text.contains("ТЕСТОВАЯ ВЕРСИЯ"), "approved report must not carry a test watermark");
            assert!(!text.contains("требуется врачебная проверка"));
            assert!(text.contains("PHYSICIAN_CONCLUSION"));
        }
    }
}

#[test]
fn ordered_diagnoses_precede_conclusion_and_escape_each_value_once() {
    let (mut detail, layout) = fixture();
    detail.protocol.as_mut().unwrap().diagnoses = Some(vec![
        row(
            2,
            "Z<&",
            "FIRST <script> & name",
            "EXTRA <tag> & text\nnext line",
        ),
        row(1, "A00", "SECOND", "SECOND_EXTRA"),
    ]);
    let native = report::native_lines(&detail, &layout, "Synthetic physician", &[])
        .unwrap()
        .join("\n");
    let output = html(&detail, &layout);
    for rendered in [&native, &output] {
        assert!(rendered.find("FIRST").unwrap() < rendered.find("EXTRA").unwrap());
        assert!(rendered.find("EXTRA").unwrap() < rendered.find("SECOND").unwrap());
        assert!(
            rendered.find("SECOND_EXTRA").unwrap() < rendered.find("PHYSICIAN_CONCLUSION").unwrap()
        );
        assert_eq!(rendered.matches("FIRST").count(), 1);
    }
    assert!(native.contains("Z<& FIRST <script> & name"));
    assert!(output.contains("Z&lt;&amp; FIRST &lt;script&gt; &amp; name"));
    assert!(output.contains("EXTRA &lt;tag&gt; &amp; text\nnext line"));
    assert!(!output.contains("<script>"));
    assert!(!output.contains("&amp;lt;"));
}

#[test]
fn absent_and_explicit_empty_diagnoses_preserve_legacy_output() {
    let (mut detail, layout) = fixture();
    let old_html = html(&detail, &layout);
    let old_native = report::native_lines(&detail, &layout, "Synthetic physician", &[]).unwrap();
    assert!(!old_html.contains("Диагнозы"));
    assert!(!old_native.iter().any(|line| line.contains("Диагнозы")));
    detail.protocol.as_mut().unwrap().diagnoses = Some(vec![]);
    assert_eq!(html(&detail, &layout), old_html);
    assert_eq!(
        report::native_lines(&detail, &layout, "Synthetic physician", &[]).unwrap(),
        old_native
    );
}

fn set_note(detail: &mut StudyDetail, value: serde_json::Value) {
    let mut protocol = serde_json::to_value(detail.protocol.as_ref().unwrap()).unwrap();
    protocol["diagnosis_note"] = serde_json::json!({"origin":{"kind":"physician"},"value":value});
    detail.protocol = Some(serde_json::from_value(protocol).unwrap());
}

#[test]
fn common_diagnosis_note_follows_all_rows_and_precedes_conclusion() {
    let (mut detail, layout) = fixture();
    detail.protocol.as_mut().unwrap().diagnoses = Some(vec![
        row(2, "Z99", "FIRST", "OLD_EXTRA"),
        row(1, "A00", "SECOND", ""),
    ]);
    set_note(
        &mut detail,
        serde_json::json!({"state":"assessed","value":"COMMON <tag> & note\nNEXT_LINE"}),
    );
    let native = report::native_lines(&detail, &layout, "Synthetic physician", &[])
        .unwrap()
        .join("\n");
    let output = html(&detail, &layout);
    for rendered in [&native, &output] {
        assert!(rendered.find("OLD_EXTRA").unwrap() < rendered.find("SECOND").unwrap());
        assert!(rendered.find("SECOND").unwrap() < rendered.find("COMMON").unwrap());
        assert!(rendered.find("COMMON").unwrap() < rendered.find("PHYSICIAN_CONCLUSION").unwrap());
        assert_eq!(rendered.matches("COMMON").count(), 1);
    }
    assert!(output.contains("COMMON &lt;tag&gt; &amp; note\nNEXT_LINE"));
    assert!(!output.contains("<tag>"));
    assert!(!output.contains("&amp;lt;"));
}

#[test]
fn common_note_is_independent_of_rows_and_empty_states_do_not_print() {
    let (mut detail, layout) = fixture();
    let baseline = html(&detail, &layout);
    let native_baseline =
        report::native_lines(&detail, &layout, "Synthetic physician", &[]).unwrap();
    for value in [
        serde_json::json!({"state":"not_assessed"}),
        serde_json::json!({"state":"assessed","value":""}),
        serde_json::json!({"state":"assessed","value":" \n "}),
    ] {
        set_note(&mut detail, value);
        assert_eq!(html(&detail, &layout), baseline);
        assert_eq!(
            report::native_lines(&detail, &layout, "Synthetic physician", &[]).unwrap(),
            native_baseline
        );
    }
    set_note(
        &mut detail,
        serde_json::json!({"state":"assessed","value":"COMMON_WITHOUT_CODES"}),
    );
    for rows in [None, Some(vec![])] {
        detail.protocol.as_mut().unwrap().diagnoses = rows;
        assert!(html(&detail, &layout).contains("COMMON_WITHOUT_CODES"));
        assert!(
            report::native_lines(&detail, &layout, "Synthetic physician", &[])
                .unwrap()
                .iter()
                .any(|line| line.contains("COMMON_WITHOUT_CODES"))
        );
    }
}

/// Disposable, entirely fictitious evidence; never reads API or patient storage.
#[cfg(windows)]
#[test]
#[ignore = "synthetic PDF proof; bundled Chromium"]
fn synthetic_diagnoses_pdf_proof() {
    let directory = std::env::var_os("M53_DIAGNOSES_PROOF_DIR")
        .expect("M53_DIAGNOSES_PROOF_DIR must name the synthetic proof output directory");
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let (mut detail, mut layout) = fixture();
    layout.blocks.push(ReportBlock::Signature);
    // Codes, UUIDs and markers deliberately oppose alphabetical order.
    detail.protocol.as_mut().unwrap().diagnoses = Some(vec![
        row(
            33,
            "Z99",
            "Диагноз вымышленный: ORDER_THIRD",
            "Первое дополнение EXTRA_FIRST",
        ),
        row(
            11,
            "A01",
            "Диагноз вымышленный: ORDER_FIRST",
            "Второе дополнение EXTRA_SECOND",
        ),
        row(
            22,
            "M55",
            "Диагноз вымышленный: ORDER_SECOND",
            "Третье дополнение EXTRA_THIRD",
        ),
    ]);
    detail.protocol.as_mut().unwrap().diagnosis_note = Some(model::field(Assessment::Assessed(
        "Общее дополнение COMMON_NOTE_A\nВторая строка COMMON_NOTE_B".into()
    )));
    let expected = [
        "Z99",
        "ORDER_THIRD",
        "EXTRA_FIRST",
        "A01",
        "ORDER_FIRST",
        "EXTRA_SECOND",
        "M55",
        "ORDER_SECOND",
        "EXTRA_THIRD",
        "COMMON_NOTE_A",
        "COMMON_NOTE_B",
        "PHYSICIAN_CONCLUSION",
    ];
    let rendered = html(&detail, &layout);
    let mut previous = 0;
    for token in expected {
        let position = rendered
            .find(token)
            .expect("synthetic marker missing from report HTML");
        assert!(
            position >= previous,
            "synthetic marker order differs: {token}"
        );
        assert_eq!(
            rendered.matches(token).count(),
            1,
            "duplicate synthetic marker: {token}"
        );
        previous = position;
    }
    let pdf = report::chromium::pdf(rendered.as_bytes()).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    let pages = report::chromium::pages(&pdf).unwrap();
    assert!(
        !pages.is_empty(),
        "synthetic PDF must contain a rendered page"
    );
    std::fs::write(directory.join("diagnoses.html"), rendered).unwrap();
    std::fs::write(directory.join("diagnoses.pdf"), pdf).unwrap();
    std::fs::write(
        directory.join("diagnoses.json"),
        serde_json::to_vec_pretty(&expected).unwrap(),
    )
    .unwrap();
    for (index, png) in pages.iter().enumerate() {
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        std::fs::write(directory.join(format!("diagnoses-{}.png", index + 1)), png).unwrap();
    }
    detail.protocol.as_mut().unwrap().colpo = None;
    let fallback = report::render_with_terms(&detail, &layout, &[], "Synthetic physician", &[]).unwrap();
    std::fs::write(directory.join("fallback.pdf"), fallback.pdf).unwrap();
}

#[test]
fn logo_free_stationery_keeps_patient_and_clinic_without_an_image_column() {
    let (detail, mut layout) = fixture();
    layout.blocks.insert(0, ReportBlock::Patient);
    layout.letterhead = Some(ReportLetterhead {
        logo_data_uri: String::new(),
        patient_rows: vec![ReportHeaderRow { label: "Пациент".into(), field: "full_name".into() }],
        clinic: Some(ReportClinic { guid: Uuid::from_u128(2), name: "Independent clinic".into(), license: String::new(), phone: String::new(), version: 1 }),
    });
    let output = html(&detail, &layout);
    assert!(!output.contains("<img alt='Логотип клиники'"));
    assert!(output.contains("<div class='letterhead'><div class='letterhead-patient'>"));
    assert_eq!(output.matches("Synthetic Test").count(), 1);
    assert!(output.contains("Independent clinic"));
    assert!(output.contains("PHYSICIAN_CONCLUSION"));
}

#[cfg(all(target_os = "linux", feature = "standalone"))]
#[test]
fn standalone_linux_renders_a_current_structured_protocol_to_pdf() {
    let (detail, layout) = fixture();
    assert!(detail.protocol.as_ref().unwrap().colpo.is_some());
    let output = report::render_with_terms(&detail, &layout, &[], "Synthetic physician", &[]).unwrap();
    assert!(output.pdf.starts_with(b"%PDF-"));
    let parsed = printpdf::lopdf::Document::load_mem(&output.pdf).unwrap();
    assert!(!parsed.get_pages().is_empty());
}
