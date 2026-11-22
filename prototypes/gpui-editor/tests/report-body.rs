#[path = "../src/verification.rs"]
mod fixture;
use m53_client::{defects, report};
use m53_contracts::workflow::{ReportBlock, ReportLayout};
use m53_domain::colpo::Answer;
use m53_gpui_editor::Photo;
use serde_json::json;
use uuid::Uuid;

#[test]
fn server_letterhead_uses_patient_snapshot_and_escapes_clinic_data() {
    use m53_contracts::workflow::{ReportClinic,ReportHeaderRow,ReportLetterhead};
    let mut detail=populated();
    detail.patient.family_name="Тест <script>".into();detail.patient.given_name="Анна".into();
    detail.patient.patronymic=None;detail.patient.birth_date=Some("1988-10-22".into());
    detail.patient.sex=Some(m53_contracts::colposcopy_1c::Sex::F);
    detail.protocol.as_mut().unwrap().colpo.as_mut().unwrap().fields.insert("context.exam_date".into(),Answer::Text("2026-10-21".into()));
    let mut layout=ReportLayout{title:"Кольпоскопия".into(),blocks:vec![ReportBlock::Patient],photo_columns:1,letterhead:Some(ReportLetterhead{
        logo_data_uri:String::new(),
        patient_rows:[("Пациент","full_name"),("Мед. карта №","card_number"),("Возраст","age"),("Пол","sex"),("Дата рождения","birth_date"),("Дата осмотра","exam_date")].into_iter().map(|(label,field)|ReportHeaderRow{label:label.into(),field:field.into()}).collect(),
        clinic:Some(ReportClinic{guid:Uuid::new_v4(),name:"Клиника <img>".into(),license:"ЛО & 123".into(),phone:"123".into(),version:1}),
    })};
    let html=String::from_utf8(report::preview_with_terms(&detail,&layout,&[],"Врач",&[]).unwrap().html).unwrap();
    for expected in ["Тест &lt;script&gt; Анна","<b>Возраст:</b> 37","Женский","22.10.1988","21.10.2026","Клиника &lt;img&gt;","ЛО &amp; 123","Телефон: 123","pageNumber","totalPages"] {assert!(html.contains(expected),"missing {expected}");}
    assert_eq!(html.matches("Тест &lt;script&gt; Анна").count(),1,"old patient block must not duplicate the letterhead");
    assert!(html.contains("<b>Мед. карта №:</b> Не указана"));
    detail.patient.card_number=Some("000123-А".into());
    let numbered=String::from_utf8(report::preview_with_terms(&detail,&layout,&[],"Врач",&[]).unwrap().html).unwrap();
    assert!(numbered.contains("<b>Мед. карта №:</b> 000123-А"));
    layout.letterhead.as_mut().unwrap().patient_rows[0].field="unsupported".into();
    assert!(report::preview_with_terms(&detail,&layout,&[],"Врач",&[]).err().unwrap().contains("Неизвестное поле"));
    layout.letterhead.as_mut().unwrap().patient_rows[0].field="full_name".into();
    layout.letterhead.as_mut().unwrap().logo_data_uri="http://localhost/secret".into();
    assert!(report::preview_with_terms(&detail,&layout,&[],"Врач",&[]).is_err());
    layout.letterhead.as_mut().unwrap().clinic=None;
    assert!(report::preview_with_terms(&detail,&layout,&[],"Врач",&[]).err().unwrap().contains("Филиал клиники не настроен"));
}

fn populated() -> m53_contracts::workflow::StudyDetail {
    let (mut detail, _, asset) = fixture::fixture();
    let asset2 = Uuid::from_u128(100);
    let mut second = detail.assets[0].clone();
    second.id = asset2;
    detail.assets.push(second);
    let p = detail.protocol.as_mut().unwrap();
    p.schema_version = 2;
    p.colpo = Some(Default::default());
    for (n, asset) in [(1, asset), (2, asset), (3, asset2)] {
        let mut photo = Photo::new(1000, 800);
        photo.id = asset;
        photo.add_callout(2, 0.2, 0.3);
        photo.annotations[0].id = Uuid::from_u128(n);
        detail.annotations.push(photo.annotations.remove(0));
    }
    defects::sync_colpo(&mut detail);
    let c = detail.protocol.as_mut().unwrap().colpo.as_mut().unwrap();
    c.marks
        .get_mut(&Uuid::from_u128(1))
        .unwrap()
        .properties
        .insert("property.mature".into(), Answer::Checked(true));
    c.marks
        .get_mut(&Uuid::from_u128(2))
        .unwrap()
        .properties
        .insert("property.atrophic".into(), Answer::Checked(true));
    c.fields
        .insert("simple.closed".into(), Answer::Checked(false));
    c.fields
        .insert("cervix.shape".into(), Answer::Choice("Коническая".into()));
    detail
}
fn body(detail: &m53_contracts::workflow::StudyDetail) -> Vec<String> {
    report::native_lines(
        detail,
        &ReportLayout {
            title: "Заголовок без изменений".into(),
            blocks: vec![ReportBlock::Assessment, ReportBlock::Findings],
            photo_columns: 1, letterhead: None,
        },
        "Тест",
        &[],
    )
    .unwrap()
}

#[test]
fn every_visible_protocol_catalog_field_reaches_report_text() {
    use m53_domain::colpo::{Control, FIELDS};
    // Adequacy/TZ controls write typed protocol fields, not their legacy IDs.
    for field in FIELDS.iter().filter(|f|f.parent.is_empty() && !matches!(f.id,"extended.adequacy"|"extended.tz")) {
        let mut detail=populated();
        let value=match field.control {
            Control::Check=>Answer::Checked(true),
            Control::Date=>Answer::Text("2026-10-21".into()),
            Control::Choice=>Answer::Choice(field.choices[0].into()),
            _=>Answer::Text(format!("Контроль {}",field.id)),
        };
        let c=detail.protocol.as_mut().unwrap().colpo.as_mut().unwrap();
        c.fields.clear();c.fields.insert(field.id.into(),value.clone());
        let text=body(&detail).concat();
        assert!(text.contains(field.label),"missing field {}: {}",field.id,text);
        if let Answer::Text(value)=value && !matches!(field.control,Control::Date) {
            assert!(text.contains(&value),"missing value {}",field.id);
        }
    }
}

#[test]
fn every_marker_property_reaches_its_report_entry() {
    use m53_domain::colpo::{Control,FIELDS,Mark};
    for field in FIELDS.iter().filter(|f|!f.parent.is_empty()) {
        let mut detail=populated();let asset=detail.assets[0].id;
        let c=detail.protocol.as_mut().unwrap().colpo.as_mut().unwrap();c.marks.clear();
        let answer=match field.control {
            Control::Check=>Answer::Checked(true),
            Control::Choice=>Answer::Choice(field.choices[0].into()),
            _=>Answer::Text("Текст свойства для отчёта".into()),
        };
        c.marks.insert(Uuid::from_u128(1000),Mark {asset_id:asset,catalog_id:field.parent.into(),properties:std::collections::BTreeMap::from([(field.id.into(),answer)])});
        assert!(body(&detail).concat().contains(field.label),"missing property {} / {}",field.parent,field.id);
    }
}

#[test]
fn typed_adequacy_reasons_and_explicit_not_applicable_zone_reach_html() {
    use m53_domain::protocol::{Assessment,Adequacy};
    let mut detail=populated();let p=detail.protocol.as_mut().unwrap();
    p.sections[0].adequacy.value=Assessment::Assessed(Adequacy::Inadequate {reason:"Причина <для проверки>".into()});
    p.cervical_assessment.transformation_zone.value=Assessment::NotApplicable;
    let layout=ReportLayout {title:"Проверка".into(),blocks:vec![ReportBlock::Assessment,ReportBlock::Findings],photo_columns: 1, letterhead: None};
    let photos=detail.assets.iter().map(|a|report::Photo {id:a.id,bytes:vec![],caption:String::new()}).collect::<Vec<_>>();
    let html=String::from_utf8(report::preview_with_terms(&detail,&layout,&photos,"Врач",&[]).unwrap().html).unwrap();
    assert!(html.contains("Причина &lt;для проверки&gt;"));
    assert!(html.contains("Зона трансформации:</b> Не применимо"));
}

#[test]
fn findings_keep_properties_and_numbering_per_photo_and_marker() {
    let detail = populated();
    let original = json!(detail);
    let rows = body(&detail);
    let text = rows.concat();
    assert!(rows.iter().any(|r| r == "Шейка матки"));
    assert!(
        rows.iter()
            .any(|r| r == "Нормальные кольпоскопические картины")
    );
    assert!(text.contains("Снимок 1, метка 1; Зрелый"));
    assert!(text.contains("Снимок 1, метка 2; Атрофический"));
    assert!(text.contains("Снимок 2, метка 1"));
    assert!(!text.contains("Закрытые железы"));
    assert!(!text.contains("Не оценено"));
    assert_eq!(json!(detail), original);
}

#[test]
fn numbered_report_labels_do_not_change_the_original_or_catalog_colors() {
    let detail = populated();
    let original = json!(detail.annotations);
    let derived = report::annotations_for_report(&detail, detail.assets[0].id);
    assert_eq!(derived[0].label, "МПЭ·1");
    assert_eq!(derived[1].label, "МПЭ·2");
    assert_eq!(
        report::annotations_for_report(&detail, detail.assets[1].id)[0].label,
        "МПЭ·1"
    );
    let preset = &m53_domain::colpo::PRESETS[2];
    assert_eq!(
        m53_domain::colpo::marker_color("МПЭ·1", preset.name, preset.code),
        preset.marker_color()
    );
    assert_eq!(json!(detail.annotations), original);
}

#[test]
fn skipped_colposcopy_keeps_vulva_and_context_without_leaking_cervical_findings() {
    let mut detail = populated();
    let c = detail.protocol.as_mut().unwrap().colpo.as_mut().unwrap();
    c.fields
        .insert("context.skip".into(), Answer::Checked(true));
    c.fields
        .insert("vulva.skin.red".into(), Answer::Checked(true));
    let text = body(&detail).concat();
    assert!(!text.contains("МПЭ"));
    assert!(!text.contains("Коническая"));
    assert!(text.contains("Оценка вульвы"));
    assert!(text.contains("Красный"));
    assert!(text.contains("Не заполнять данные кольпоскопии"));
}

#[test]
fn only_selected_photo_contributes_marks_and_properties_without_renumbering() {
    let detail = populated();
    let original = json!(detail);
    let selected = report::select_report_photos(&detail, &[detail.assets[1].id]);
    let text = body(&selected).concat();
    assert!(!text.contains("Снимок 1"));
    assert!(text.contains("Снимок 2, метка 1"));
    assert!(!text.contains("Зрелый"));
    assert!(!text.contains("Атрофический"));
    assert_eq!(selected.annotations.len(), 1);
    assert_eq!(
        selected
            .protocol
            .as_ref()
            .unwrap()
            .colpo
            .as_ref()
            .unwrap()
            .marks
            .len(),
        1
    );
    assert_eq!(json!(detail), original);
    // Selecting it again restores the untouched report view and its per-marker properties.
    assert!(
        body(&report::select_report_photos(
            &detail,
            &[detail.assets[0].id]
        ))
        .concat()
        .contains("Зрелый")
    );
}

#[test]
fn empty_selection_hides_region_fields_without_changing_original() {
    let detail = populated();
    let selected = report::select_report_photos(&detail, &[]);
    let text = body(&selected).concat();
    assert!(!text.contains("Коническая"));
    assert!(body(&detail).concat().contains("Коническая"));
    assert!(!text.contains("МПЭ"));
    assert!(!text.contains("метка"));
    assert!(selected.annotations.is_empty());
}

#[test]
fn html_export_and_native_preview_use_the_same_selection() {
    let detail = populated();
    let layout = ReportLayout {
        title: "Тест".into(),
        blocks: vec![ReportBlock::Findings],
        photo_columns: 1, letterhead: None,
    };
    let selected = detail.assets[1].id;
    let preview = report::native_lines_for_photos(&detail, &layout, "Тест", &[], &[selected])
        .unwrap()
        .concat();
    let html = report::preview_with_terms(
        &detail,
        &layout,
        &[report::Photo {
            id: selected,
            bytes: vec![],
            caption: "Снимок 2".into(),
        }],
        "Тест",
        &[],
    )
    .unwrap();
    let html = String::from_utf8(html.html).unwrap();
    for text in [&preview, &html] {
        assert!(text.contains("Снимок 2, метка 1"));
        assert!(!text.contains("Снимок 1"));
        assert!(!text.contains("Зрелый"));
    }
}

#[test]
fn html_report_has_real_tables_bold_designations_and_escaped_input() {
    let mut detail = populated();
    detail.protocol.as_mut().unwrap().conclusion.value = m53_domain::protocol::Assessment::Assessed(
        "<script>не выполнять</script>\nВторая строка".into(),
    );
    let layout = ReportLayout {
        title: "Протокол".into(),
        blocks: vec![
            ReportBlock::Patient,
            ReportBlock::Assessment,
            ReportBlock::Findings,
            ReportBlock::Conclusion,
        ],
        photo_columns: 1, letterhead: None,
    };
    let photos = detail
        .assets
        .iter()
        .map(|a| report::Photo {
            id: a.id,
            bytes: vec![],
            caption: String::new(),
        })
        .collect::<Vec<_>>();
    let rendered = report::preview_with_terms(&detail, &layout, &photos, "Тест", &[]).unwrap();
    let html = String::from_utf8(rendered.html).unwrap();
    assert!(html.contains("colspan='3'>Шейка матки"));
    assert!(html.contains("<th>Нормальные кольпоскопические картины</th><th>Аномальные"));
    assert!(html.contains("<b>МПЭ</b>"));
    assert!(html.contains("&lt;script&gt;"));
    assert!(!html.contains("<script>"));
    assert!(html.contains("сн. 1: 1"));
    assert!(html.contains("сн. 1: 2")); // differing properties are not merged
    assert!(html.contains("сн. 2: 1"));
}

#[cfg(windows)]
#[test]
fn chromium_prints_and_windows_renders_paginated_tables() {
    let mut detail = populated();
    detail.protocol.as_mut().unwrap().conclusion.value = m53_domain::protocol::Assessment::Assessed(
        (1..=80)
            .map(|i| format!("Строка {i}. Проверка переноса заключения без потери текста."))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let layout = ReportLayout {
        title: "Синтетический протокол".into(),
        blocks: vec![
            ReportBlock::Patient,
            ReportBlock::Assessment,
            ReportBlock::Findings,
            ReportBlock::Conclusion,
            ReportBlock::Photos,
        ],
        photo_columns: 1, letterhead: None,
    };
    let mut image_bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
        400,
        300,
        image::Rgb([240, 235, 225]),
    ))
    .write_to(&mut image_bytes, image::ImageFormat::Png)
    .unwrap();
    let photos = detail
        .assets
        .iter()
        .map(|a| report::Photo {
            id: a.id,
            bytes: image_bytes.get_ref().clone(),
            caption: String::new(),
        })
        .collect::<Vec<_>>();
    let html = report::preview_with_terms(&detail, &layout, &photos, "Синтетический врач", &[])
        .unwrap()
        .html;
    let pdf = report::chromium::pdf(&html).unwrap();
    let pages = report::chromium::pages(&pdf).unwrap();
    assert!(pages.len() >= 4);
    for page in &pages {
        assert!(page.starts_with(b"\x89PNG"));
    }
    if let Some(path) = std::env::var_os("M53_REPORT_QA_DIR") {
        let path = std::path::PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("tables.html"), html).unwrap();
        std::fs::write(path.join("tables.pdf"), pdf).unwrap();
        for (i, page) in pages.iter().enumerate() {
            std::fs::write(path.join(format!("page-{}.png", i + 1)), page).unwrap();
        }
    }
}

#[test]
fn vulva_groups_keep_their_findings_and_conclusion_moves_as_a_block() {
    let mut detail=populated();
    let mut photo=Photo::new(1000,800);photo.id=detail.assets[0].id;
    for id in ["vulva.normal.micro","vulva.morph.excoriation","vulva.cancer.ulceration"] {
        let ix=defects::PRESETS.iter().position(|p| p.id==id).unwrap();
        photo.add_callout(ix,0.4,0.4);
    }
    detail.annotations.extend(photo.annotations);defects::sync_colpo(&mut detail);
    detail.protocol.as_mut().unwrap().conclusion.value=m53_domain::protocol::Assessment::Assessed(
        (1..=40).map(|i|format!("Строка заключения {i}: проверка цельного блока.")).collect::<Vec<_>>().join("\n"));
    let layout=ReportLayout{title:"Проверка групп вульвы".into(),blocks:vec![ReportBlock::Patient,ReportBlock::Assessment,ReportBlock::Findings,ReportBlock::Conclusion],photo_columns: 1, letterhead: None};
    let photos=detail.assets.iter().map(|a|report::Photo{id:a.id,bytes:vec![],caption:String::new()}).collect::<Vec<_>>();
    let html=String::from_utf8(report::preview_with_terms(&detail,&layout,&photos,"Тест",&[]).unwrap().html).unwrap();
    let vulva=html.split("class='section vulva'").nth(1).unwrap();
    let normal=vulva.find("Нормальные картины:").unwrap();let micro=vulva.find("Микропапилломатоз").unwrap();
    let morph=vulva.find("Вторичная морфология:").unwrap();let exc=vulva.find("Экскориация").unwrap();
    let cancer=vulva.find("Подозрение на рак:").unwrap();let ulcer=vulva.find("Изъязвление").unwrap();
    assert!(normal<micro && micro<morph && morph<exc && exc<cancer && cancer<ulcer);
    assert!(html.contains(".conclusion { break-inside:avoid;"));
    #[cfg(windows)] {
        let pdf=report::chromium::pdf(html.as_bytes()).unwrap();let pages=report::chromium::pages(&pdf).unwrap();
        assert_eq!(pages.len(),2,"40-line conclusion must fit whole on the second page");
        if let Some(path)=std::env::var_os("M53_REPORT_QA_DIR") {
            let path=std::path::PathBuf::from(path);std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("grouped-vulva.html"),html).unwrap();std::fs::write(path.join("grouped-vulva.pdf"),pdf).unwrap();
            for (i,page) in pages.iter().enumerate() {std::fs::write(path.join(format!("grouped-vulva-{}.png",i+1)),page).unwrap();}
        }
    }
}

#[test]
fn deselecting_cervix_photo_hides_whole_region_in_html_and_native_view() {
    let mut detail=populated();let original=json!(detail);
    let mut photo=Photo::new(1000,800);photo.id=detail.assets[1].id;
    let ix=defects::PRESETS.iter().position(|p| p.id=="vulva.normal.micro").unwrap();
    photo.add_callout(ix,0.4,0.4);
    detail.annotations.retain(|a| a.asset_id!=photo.id);detail.annotations.extend(photo.annotations);defects::sync_colpo(&mut detail);
    let layout=ReportLayout{title:"Тест".into(),blocks:vec![ReportBlock::Assessment,ReportBlock::Findings],photo_columns: 1, letterhead: None};
    let selected=detail.assets[1].id;
    let html=String::from_utf8(report::preview_with_terms(&detail,&layout,&[report::Photo{id:selected,bytes:vec![],caption:String::new()}],"Тест",&[]).unwrap().html).unwrap();
    let native=report::native_lines_for_photos(&detail,&layout,"Тест",&[],&[selected]).unwrap().concat();
    for text in [&html,&native] {
        assert!(text.contains("Микропапилломатоз"));
        assert!(!text.contains("Шейка матки"));assert!(!text.contains("Простая кольпоскопия"));
        assert!(!text.contains("Нормальные кольпоскопические картины"));assert!(!text.contains("Коническая"));
    }
    let restored=report::select_report_photos(&detail,&[detail.assets[0].id]);
    assert!(body(&restored).concat().contains("Коническая"));
    assert_eq!(json!(detail.protocol.as_ref().unwrap().colpo.as_ref().unwrap().fields),original["protocol"]["colpo"]["fields"]);
}

#[test]
fn each_photo_has_its_own_legend_and_properties() {
    let detail=populated();
    let mut bytes=std::io::Cursor::new(vec![]);
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(400,300,image::Rgb([235,235,235])))
        .write_to(&mut bytes,image::ImageFormat::Png).unwrap();
    let photos=detail.assets.iter().map(|a|report::Photo{id:a.id,bytes:bytes.get_ref().clone(),caption:String::new()}).collect::<Vec<_>>();
    let layout=ReportLayout{title:"Описание снимков".into(),blocks:vec![ReportBlock::Photos],photo_columns: 1, letterhead: None};
    let html=String::from_utf8(report::preview_with_terms(&detail,&layout,&photos,"Тест",&[]).unwrap().html).unwrap();
    let pages=html.split("<section class='photo-page'>").skip(1).collect::<Vec<_>>();
    assert_eq!(pages.len(),2);
    assert!(pages[0].contains("Зрелый") && pages[0].contains("Атрофический"));
    assert!(!pages[1].contains("Зрелый") && !pages[1].contains("Атрофический"));
    for page in &pages {assert!(!page.contains("сн."));assert!(!page.contains("Снимок 1, метка"));assert!(!page.contains("Снимок 2, метка"));}
    assert!(pages[0].contains("Многослойный плоский эпителий (<b>МПЭ</b>)"));
    #[cfg(windows)] if let Some(path)=std::env::var_os("M53_REPORT_QA_DIR") {
        let path=std::path::PathBuf::from(path);std::fs::create_dir_all(&path).unwrap();
        let pdf=report::chromium::pdf(html.as_bytes()).unwrap();let pages=report::chromium::pages(&pdf).unwrap();
        assert_eq!(pages.len(),3); // document header, then two photo sheets
        std::fs::write(path.join("photo-legends.pdf"),pdf).unwrap();
        for (i,page) in pages.iter().enumerate(){std::fs::write(path.join(format!("photo-legend-{}.png",i+1)),page).unwrap();}
    }
}

#[test]
fn equipment_model_is_printed_once_escaped_and_does_not_change_study() {
    let detail=populated();let original=json!(detail);
    let layout=ReportLayout{title:"Тест".into(),blocks:vec![ReportBlock::Findings],photo_columns: 1, letterhead: None};
    let rendered=report::with_equipment(&detail,&layout,&[],"Врач",&[],Some("Sensitec <VC-200> & тест"),true).unwrap();
    let html=String::from_utf8(rendered.html).unwrap();
    assert_eq!(html.matches("Кольпоскоп:").count(),1);
    assert!(html.contains("Sensitec &lt;VC-200&gt; &amp; тест"));
    assert!(!html.contains("<VC-200>"));
    assert_eq!(json!(detail),original);
}
