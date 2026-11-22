//! Reports are rendered on the client. No HTML input or executable template code.
use base64::{Engine, engine::general_purpose::STANDARD};
use m53_contracts::workflow::*;
use m53_domain::protocol::*;
use printpdf::{Image, ImageTransform, Mm, PdfDocument};
use std::io::Cursor;
use uuid::Uuid;

/// Bump when PDF output semantics or the bundled renderer changes.
#[cfg(not(all(target_os = "linux", feature = "standalone")))]
pub const RENDERER_ID: &str = "chromium-156.0.8078.47-pdfjs-6.4.299-v10";
#[cfg(all(target_os = "linux", feature = "standalone"))]
pub const RENDERER_ID: &str = "linux-printpdf-v1";

#[cfg(all(target_os = "linux", feature = "standalone"))]
pub fn linux_pdf_pages(bytes: &[u8]) -> Result<usize, String> {
    let document = printpdf::lopdf::Document::load_mem(bytes).map_err(|_| "Некорректный PDF")?;
    let count = document.get_pages().len();
    if !(1..=128).contains(&count) {
        return Err("Для предпросмотра поддерживается от 1 до 128 страниц".into());
    }
    Ok(count)
}

#[cfg(windows)]
pub mod chromium;
mod colpo_body;
mod html;
mod letterhead;
pub use colpo_body::mark_number;
pub fn is_body_heading(line: &str) -> bool {
    colpo_body::HEADINGS.contains(&line)
}

/// Derive the report view without changing the study or renumbering its original photos.
pub fn select_report_photos(detail: &StudyDetail, selected: &[Uuid]) -> StudyDetail {
    let selected: std::collections::BTreeSet<_> = detail
        .assets
        .iter()
        .filter(|a| a.kind == "original" && selected.contains(&a.id))
        .map(|a| a.id)
        .collect();
    let excluded_regions: Vec<_> = [Region::Cervix, Region::Vulva, Region::Anus]
        .into_iter()
        .filter(|region| {
            let marks = detail.protocol.as_ref().and_then(|p| p.colpo.as_ref());
            let belongs = |m: &m53_domain::colpo::Mark| {
                m53_domain::colpo::preset(&m.catalog_id).is_some_and(|p| p.region == *region)
            };
            marks.is_some_and(|c| {
                c.marks.values().any(belongs)
                    && !c
                        .marks
                        .values()
                        .any(|m| belongs(m) && selected.contains(&m.asset_id))
            })
        })
        .collect();
    let mut filtered = detail.clone();
    filtered
        .annotations
        .retain(|a| selected.contains(&a.asset_id));
    let annotations: std::collections::BTreeSet<_> =
        filtered.annotations.iter().map(|a| a.id).collect();
    if let Some(p) = &mut filtered.protocol {
        if let Some(c) = &mut p.colpo {
            c.marks
                .retain(|id, m| selected.contains(&m.asset_id) && annotations.contains(id));
            c.mark_history.clear();
            c.fields.retain(|key, _| {
                !(excluded_regions.contains(&Region::Cervix)
                    && ["cervix.", "simple.", "extended.", "rectovaginal."]
                        .iter()
                        .any(|prefix| key.starts_with(prefix)))
                    && !(excluded_regions.contains(&Region::Vulva) && key.starts_with("vulva."))
            });
        }
        p.sections.retain(|s| !excluded_regions.contains(&s.region));
        if excluded_regions.contains(&Region::Cervix) {
            p.cervical_assessment.transformation_zone.value = Assessment::NotAssessed;
        }
        let keep = |f: &Finding| match f.observation.origin {
            Origin::Annotation { annotation_id } => annotations.contains(&annotation_id),
            _ => true,
        };
        for section in &mut p.sections {
            section.findings.retain(keep);
            for lesion in &mut section.lesions {
                lesion.findings.retain(keep);
            }
        }
    }
    filtered
}

/// Number the derived image labels only; the stored annotation and original remain intact.
pub fn annotations_for_report(detail: &StudyDetail, asset: Uuid) -> Vec<Annotation> {
    detail
        .annotations
        .iter()
        .filter(|a| a.asset_id == asset)
        .cloned()
        .map(|mut a| {
            if matches!(a.layer, AnnotationLayer::Callout { .. })
                && let Some(number) = detail
                    .protocol
                    .as_ref()
                    .and_then(|p| p.colpo.as_ref())
                    .and_then(|c| mark_number(c, a.id))
            {
                a.label = format!("{}·{number}", a.label);
            }
            a
        })
        .collect()
}

pub struct Photo {
    pub id: Uuid,
    pub bytes: Vec<u8>,
    pub caption: String,
}
pub struct Rendered {
    pub html: Vec<u8>,
    pub pdf: Vec<u8>,
}
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn text(field: &Field<Assessment<String>>) -> String {
    match &field.value {
        Assessment::Assessed(s) => s.clone(),
        Assessment::NotAssessed => "Не оценено".into(),
        Assessment::NotApplicable => "Не применимо".into(),
    }
}
fn region(region: Region) -> &'static str {
    match region {
        Region::Cervix => "Шейка матки",
        Region::Vagina => "Влагалище",
        Region::Vulva => "Вульва",
        Region::Anus => "Анус",
    }
}
fn state(state: m53_domain::FeatureState) -> &'static str {
    match state {
        m53_domain::FeatureState::Present => "Присутствует",
        m53_domain::FeatureState::Absent => "Отсутствует",
        m53_domain::FeatureState::NotAssessed => "Не оценено",
        m53_domain::FeatureState::NotApplicable => "Не применимо",
    }
}
fn assessment<T: std::fmt::Debug>(value: &Assessment<T>) -> String {
    match value {
        Assessment::Assessed(v) => format!("{v:?}"),
        Assessment::NotAssessed => "Не оценено".into(),
        Assessment::NotApplicable => "Не применимо".into(),
    }
}
fn report_blocks(layout: &ReportLayout) -> Vec<ReportBlock> {
    let mut blocks = Vec::new();
    let mut history = false;
    for block in &layout.blocks {
        if matches!(block, ReportBlock::History) {
            if history { continue; }
            history = true;
        } else if !history && matches!(block, ReportBlock::Assessment | ReportBlock::Findings | ReportBlock::Conclusion) {
            blocks.push(ReportBlock::History);
            history = true;
        }
        blocks.push(block.clone());
    }
    if !history { blocks.push(ReportBlock::History); }
    blocks
}

fn lines(
    block: &ReportBlock,
    detail: &StudyDetail,
    p: &Protocol,
    author: &str,
    labels: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    if let Some(colpo) = &p.colpo {
        if matches!(block, ReportBlock::Assessment) {
            return colpo_body::assessment(detail, p, colpo);
        }
        if matches!(block, ReportBlock::Findings) {
            return colpo_body::findings(detail, p, colpo);
        }
        if matches!(
            block,
            ReportBlock::Indications | ReportBlock::Recommendations
        ) {
            return vec![];
        }
    }
    match block {
        ReportBlock::Patient => vec![
            format!(
                "Пациент: {} {} {}",
                detail.patient.family_name,
                detail.patient.given_name,
                detail.patient.patronymic.as_deref().unwrap_or("")
            ),
            format!(
                "Дата рождения: {}",
                detail.patient.birth_date.as_deref().unwrap_or("Не указана")
            ),
            format!(
                "Внешний ID: {}",
                detail
                    .patient
                    .external_id
                    .as_deref()
                    .unwrap_or("Не передан внешней системой")
            ),
        ],
        ReportBlock::Indications => vec![format!("Показания: {}", text(&p.indications))],
        ReportBlock::History => match &p.history.value {
            Assessment::Assessed(value) if !value.trim().is_empty() => vec![format!("Анамнез: {value}")],
            _ => vec![],
        },
        ReportBlock::Assessment => {
            let mut out = vec!["Общая оценка".into()];
            for s in &p.sections {
                out.push(format!(
                    "{}: {}",
                    region(s.region),
                    match s.state.value {
                        m53_domain::ExaminationState::Examined => "Исследовано",
                        m53_domain::ExaminationState::NotExamined => "Не исследовано",
                        m53_domain::ExaminationState::NotApplicable => "Не применимо",
                    }
                ));
                if s.state.value == m53_domain::ExaminationState::Examined {
                    out.push(format!(
                        "Адекватность: {}",
                        match &s.adequacy.value {
                            Assessment::Assessed(Adequacy::Adequate) => "Адекватно".into(),
                            Assessment::Assessed(Adequacy::Inadequate { reason }) =>
                                format!("Неадекватно: {reason}"),
                            _ => assessment(&s.adequacy.value),
                        }
                    ));
                }
            }
            out.push(format!(
                "Видимость стыка: {}",
                assessment(&p.cervical_assessment.junction_visibility.value)
            ));
            out.push(format!(
                "Зона трансформации шейки: {}",
                assessment(&p.cervical_assessment.transformation_zone.value)
            ));
            out
        }
        ReportBlock::Findings => {
            let mut out = vec!["Находки и очаги".into()];
            for s in &p.sections {
                for f in &s.findings {
                    out.push(format!(
                        "{} / {}: {}",
                        region(s.region),
                        labels.get(&f.term_code).unwrap_or(&f.term_code),
                        state(f.observation.value)
                    ));
                }
                for (i, l) in s.lesions.iter().enumerate() {
                    out.push(format!("{}: очаг {} ({})", region(s.region), i + 1, l.id));
                    match &l.location {
                        Location::Cervix {
                            zone_relation,
                            clock_start,
                            clock_end,
                            quadrants,
                            affected_percent,
                        } => out.push(format!(
                            "Положение: {}; часы {}–{}; квадранты {}; поражение {}%",
                            assessment(&zone_relation.value),
                            assessment(&clock_start.value),
                            assessment(&clock_end.value),
                            assessment(&quadrants.value),
                            assessment(&affected_percent.value)
                        )),
                        Location::Vagina {
                            thirds,
                            walls,
                            zone_relation,
                        } => out.push(format!(
                            "Треть {}; стенка {}; положение {}",
                            assessment(&thirds.value),
                            assessment(&walls.value),
                            assessment(&zone_relation.value)
                        )),
                        Location::Vulva {
                            anatomy_code,
                            tissue,
                            length_cm,
                            width_cm,
                        } => out.push(format!(
                            "Локализация {}; ткань {}; размер {} × {} см",
                            assessment(&anatomy_code.value),
                            assessment(&tissue.value),
                            assessment(&length_cm.value),
                            assessment(&width_cm.value)
                        )),
                        Location::Anus {
                            anatomy_code,
                            dentate_line_relation,
                            length_cm,
                            width_cm,
                        } => out.push(format!(
                            "Локализация {}; зубчатая линия {}; размер {} × {} см",
                            assessment(&anatomy_code.value),
                            assessment(&dentate_line_relation.value),
                            assessment(&length_cm.value),
                            assessment(&width_cm.value)
                        )),
                    }
                    for f in &l.findings {
                        out.push(format!(
                            "{}: {}",
                            labels.get(&f.term_code).unwrap_or(&f.term_code),
                            state(f.observation.value)
                        ));
                    }
                }
            }
            for t in &p.tests {
                out.push(format!(
                    "Проба {} / {}: {:?}; результат {}",
                    region(t.region),
                    t.code,
                    t.application.value,
                    text(&t.result)
                ));
            }
            for b in &p.biopsies {
                out.push(format!(
                    "Материал / {}: {}; локализация {}; направление на гистологию: {}",
                    region(b.region),
                    text(&b.material),
                    text(&b.location),
                    state(b.sent_for_histology.value)
                ));
            }
            for s in &p.excision.specimens {
                out.push(format!(
                    "Эксцизия: {}; длина {} мм; толщина {} мм; окружность {} мм",
                    assessment(&s.excision_type.value),
                    assessment(&s.length_mm.value),
                    assessment(&s.thickness_mm.value),
                    assessment(&s.circumference_mm.value)
                ));
            }
            out
        }
        ReportBlock::Photos => vec![],
        ReportBlock::Conclusion => {
            let mut out = Vec::new();
            if let Some(rows) = p.diagnoses.as_ref().filter(|rows| !rows.is_empty()) {
                out.push("Диагнозы".into());
                out.extend(rows.iter().map(|row| {
                    let mut entry = format!("{} {}", row.code, row.name);
                    if !row.additional_text.is_empty() {
                        entry.push('\n');
                        entry.push_str(&row.additional_text);
                    }
                    entry
                }));
            }
            if let Some(note) = p.diagnosis_note.as_ref()
                && let Assessment::Assessed(value) = &note.value
                && !value.trim().is_empty()
            {
                out.push(value.clone());
            }
            out.push(format!("Заключение: {}", text(&p.conclusion)));
            out
        },
        ReportBlock::Recommendations => vec![format!("Рекомендации: {}", text(&p.recommendations))],
        ReportBlock::Signature => vec![format!("Врач: {author}")],
    }
}
pub fn render_with_terms(
    detail: &StudyDetail,
    layout: &ReportLayout,
    photos: &[Photo],
    author: &str,
    terms: &[m53_domain::validation::Term],
) -> Result<Rendered, String> {
    render_internal(detail, layout, photos, author, terms, false, None)
}
pub fn preview_with_terms(
    detail: &StudyDetail,
    layout: &ReportLayout,
    photos: &[Photo],
    author: &str,
    terms: &[m53_domain::validation::Term],
) -> Result<Rendered, String> {
    render_internal(detail, layout, photos, author, terms, true, None)
}
pub fn with_equipment(
    detail: &StudyDetail,
    layout: &ReportLayout,
    photos: &[Photo],
    author: &str,
    terms: &[m53_domain::validation::Term],
    model: Option<&str>,
    preview: bool,
) -> Result<Rendered, String> {
    render_internal(detail, layout, photos, author, terms, preview, model)
}
fn render_internal(
    detail: &StudyDetail,
    layout: &ReportLayout,
    photos: &[Photo],
    author: &str,
    terms: &[m53_domain::validation::Term],
    preview: bool,
    equipment: Option<&str>,
) -> Result<Rendered, String> {
    let selected: Vec<_> = photos.iter().map(|photo| photo.id).collect();
    let report_detail = select_report_photos(detail, &selected);
    let detail = &report_detail;
    let labels = terms
        .iter()
        .map(|t| (t.code.clone(), t.label.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let p = detail
        .protocol
        .as_ref()
        .ok_or("Сначала сохраните протокол")?;
    let structured_html: Option<Vec<u8>> = if p.colpo.is_some() {
        let mut html = html::render(detail, layout, photos, author, &labels)?;
        if let Some(model) = equipment.filter(|s| !s.trim().is_empty()) {
            let text = String::from_utf8(html).map_err(|e| e.to_string())?;
            html = text
                .replacen(
                    "</h1>",
                    &format!("</h1><p>Кольпоскоп: {}</p>", escape(model)),
                    1,
                )
                .into_bytes();
        }
        if preview {
            return Ok(Rendered { html, pdf: vec![] });
        }
        #[cfg(windows)]
        {
            let pdf = chromium::pdf(&html)?;
            return Ok(Rendered { html, pdf });
        }
        #[cfg(not(any(windows, all(target_os = "linux", feature = "standalone"))))]
        return Err("HTML-отчет печатается клиентом Windows через Chromium".into());
        #[cfg(all(target_os = "linux", feature = "standalone"))]
        Some(html)
    } else {
        None
    };
    if !(1..=3).contains(&layout.photo_columns) || layout.blocks.len() > 20 {
        return Err("Некорректный шаблон".into());
    }
    let (doc, mut page, mut layer) =
        PdfDocument::new(&layout.title, Mm(210.0), Mm(297.0), "Report");
    let doc = doc.with_conformance(printpdf::PdfConformance::Custom(
        printpdf::CustomPdfConformance::default(),
    ));
    let font = doc
        .add_external_font(Cursor::new(
            include_bytes!("../assets/DejaVuSans.ttf").as_slice(),
        ))
        .map_err(|_| "Ошибка шрифта")?;
    let mut y = 280.0f32;
    let mut html = format!(
        "<!doctype html><html lang=\"ru\"><meta charset=\"utf-8\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'\"><title>{}</title><style>body{{font:12pt sans-serif;margin:18mm}}h1{{font-size:18pt}}p{{white-space:pre-wrap;overflow-wrap:anywhere}}.photos{{display:grid;grid-template-columns:repeat({},1fr);gap:8mm}}img{{width:100%;height:auto}}figure{{break-inside:avoid;margin:0}}.photos{{display:block}}.photo-page{{break-before:page;page-break-before:always;min-height:250mm;padding-top:8mm}}.photo-page img{{display:block;max-height:235mm;object-fit:contain}}@media screen{{.photo-page{{border-top:1px solid #ccc;margin-top:24mm}}}}@page{{size:A4;margin:0}}@media screen{{body{{margin:0;padding:8px;background:#888}}.text-page,.photo-page{{background:white;color:black;box-sizing:border-box;padding:18mm;min-height:297mm}}.photo-page{{margin-top:18px}}}}@media print{{body{{margin:0}}.text-page,.photo-page{{padding:15mm;box-sizing:border-box}}}}</style><section class=\"text-page\"><h1>{}</h1>",
        escape(&layout.title),
        1,
        escape(&layout.title)
    );
    write_line(&doc, &font, &mut page, &mut layer, &mut y, &layout.title);
    let text_letterhead = structured_html.is_some() && layout.letterhead.is_some();
    #[cfg(all(target_os = "linux", feature = "standalone"))]
    if text_letterhead {
        for line in letterhead::text_lines(detail, layout)? {
            write_line(&doc, &font, &mut page, &mut layer, &mut y, &line);
        }
        if let Some(model) = equipment.filter(|text| !text.trim().is_empty()) {
            write_line(&doc, &font, &mut page, &mut layer, &mut y, &format!("Кольпоскоп: {model}"));
        }
    }
    let blocks = report_blocks(layout);
    let ordered: Vec<_> = blocks
        .iter()
        .filter(|b| !matches!(b, ReportBlock::Photos))
        .chain(
            blocks
                .iter()
                .filter(|b| matches!(b, ReportBlock::Photos)),
        )
        .collect();
    let callouts: Vec<_> = detail
        .annotations
        .iter()
        .filter_map(|a| {
            if p.colpo.is_some() {
                return None;
            }
            if let AnnotationLayer::Callout { ref full_name, .. } = a.layer {
                Some((a, full_name))
            } else {
                None
            }
        })
        .collect();
    if !callouts.is_empty() {
        html.push_str("<h2>Обозначения на снимках</h2>");
        write_line(
            &doc,
            &font,
            &mut page,
            &mut layer,
            &mut y,
            "Обозначения на снимках",
        );
        for (a, name) in callouts {
            let number = detail
                .assets
                .iter()
                .filter(|a| a.kind == "original")
                .position(|asset| asset.id == a.asset_id)
                .map(|i| (i + 1).to_string())
                .unwrap_or_else(|| "—".into());
            let line = format!("Снимок {number}: {} — {name}", a.label);
            html.push_str(&format!("<p>{}</p>", escape(&line)));
            write_line(&doc, &font, &mut page, &mut layer, &mut y, &line);
        }
    }
    let mut text_open = true;
    for (block_index, block) in ordered.iter().enumerate() {
        if text_letterhead && matches!(block, ReportBlock::Patient) {
            continue;
        }
        if matches!(block, ReportBlock::Photos) {
            if text_open {
                html.push_str("</section>");
                text_open = false;
            }
            html.push_str("<div class=\"photos\">");
            let columns = 1usize;
            for (i, photo) in photos.iter().enumerate() {
                let caption = if detail
                    .annotations
                    .iter()
                    .any(|a| a.asset_id == photo.id && !a.label.is_empty())
                {
                    format!(
                        "{}: {}",
                        photo.caption,
                        detail
                            .annotations
                            .iter()
                            .filter(|a| a.asset_id == photo.id && !a.label.is_empty())
                            .map(|a| a.label.clone())
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                } else {
                    photo.caption.clone()
                };
                let image = crate::imaging::decode(&photo.bytes)?;
                let normalized =
                    crate::imaging::annotated(&image, &annotations_for_report(detail, photo.id));
                let display = if preview {
                    normalized.thumbnail(1400, 1800)
                } else {
                    normalized.clone()
                };
                let (mime, bytes) = if preview {
                    let mut bytes = vec![];
                    display
                        .write_to(
                            &mut Cursor::new(&mut bytes),
                            image::ImageOutputFormat::Jpeg(82),
                        )
                        .map_err(|_| "Ошибка изображения предпросмотра")?;
                    ("jpeg", bytes)
                } else {
                    ("png", crate::imaging::png(&display)?)
                };
                html.push_str(&format!("<figure class=\"photo-page\"><img alt=\"{}\" src=\"data:image/{};base64,{}\"><figcaption>{}</figcaption></figure>",escape(&caption),mime,STANDARD.encode(bytes),escape(&caption)));
                if preview {
                    continue;
                }
                // Photos occupy explicit pages, preserving their aspect ratio.
                if i % columns == 0 {
                    let next = doc.add_page(Mm(210.0), Mm(297.0), "Photos");
                    page = next.0;
                    layer = next.1;
                }
                let width = 180.0 / columns as f32 - 4.0;
                let height = width * normalized.height() as f32 / normalized.width() as f32;
                let scale = (220.0 / height).min(1.0);
                let width = width * scale;
                let height = height * scale;
                let scale_x = width / (normalized.width() as f32 * 25.4 / 300.0);
                let scale_y = height / (normalized.height() as f32 * 25.4 / 300.0);
                Image::from_dynamic_image(&normalized).add_to_layer(
                    doc.get_page(page).get_layer(layer),
                    ImageTransform {
                        translate_x: Some(Mm(
                            15.0 + (i % columns) as f32 * (180.0 / columns as f32)
                        )),
                        translate_y: Some(Mm(270.0 - height)),
                        scale_x: Some(scale_x),
                        scale_y: Some(scale_y),
                        dpi: Some(300.0),
                        ..Default::default()
                    },
                );
                doc.get_page(page).get_layer(layer).use_text(
                    caption.chars().take(40).collect::<String>(),
                    8.0,
                    Mm(15.0 + (i % columns) as f32 * (180.0 / columns as f32)),
                    Mm(260.0 - height),
                    &font,
                );
            }
            html.push_str("</div>");
            if !photos.is_empty() && block_index + 1 < layout.blocks.len() {
                let next = doc.add_page(Mm(210.0), Mm(297.0), "Text");
                page = next.0;
                layer = next.1;
                y = 280.0;
            }
        } else {
            let section = lines(block, detail, p, author, &labels);
            for line in &section {
                if is_body_heading(line) {
                    html.push_str(&format!("<h2>{}</h2>", escape(line)));
                } else {
                    if matches!(block, ReportBlock::Signature) {
                        html.push_str(&format!("<p class='physician-signature' style='text-align:right'>{}</p>", escape(line)));
                    } else { html.push_str(&format!("<p>{}</p>", escape(line))); }
                }
                write_line_aligned(&doc, &font, &mut page, &mut layer, &mut y, line, matches!(block, ReportBlock::Signature));
            }
            y -= 3.0;
        }
    }
    if text_open {
        html.push_str("</section>");
    }
    html.push_str("</html>");
    if preview {
        return Ok(Rendered {
            html: html.into_bytes(),
            pdf: vec![],
        });
    }
    let raw = doc.save_to_bytes().map_err(|_| "Ошибка формирования PDF")?;
    let mut parsed =
        printpdf::lopdf::Document::load_mem(&raw).map_err(|_| "Ошибка проверки PDF")?;
    if let Ok(root) = parsed.trailer.get(b"Root").and_then(|o| o.as_reference())
        && let Ok(catalog) = parsed.get_object_mut(root).and_then(|o| o.as_dict_mut())
    {
        catalog.remove(b"OutputIntents");
    }
    if let Ok(info) = parsed.trailer.get(b"Info").and_then(|o| o.as_reference())
        && let Ok(dictionary) = parsed.get_object_mut(info).and_then(|o| o.as_dict_mut())
    {
        dictionary.remove(b"GTS_PDFXVersion");
        dictionary.remove(b"GTS_PDFXConformance");
        let mut title = vec![0xfe, 0xff];
        for code in layout.title.encode_utf16() {
            title.extend_from_slice(&code.to_be_bytes());
        }
        dictionary.set(
            "Title",
            printpdf::lopdf::Object::String(title, printpdf::lopdf::StringFormat::Hexadecimal),
        );
    }
    let mut pdf = vec![];
    parsed
        .save_to(&mut pdf)
        .map_err(|_| "Ошибка сохранения PDF")?;
    Ok(Rendered {
        html: structured_html.unwrap_or_else(|| html.into_bytes()),
        pdf,
    })
}

/// Shared line breaking for native pages and the Rust PDF compositor.
pub fn wrap_report_line(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut wrapped = String::new();
    let mut count = 0;
    for c in line.chars().chain(std::iter::once('\n')) {
        if c == '\n' || count == 60 {
            out.push(std::mem::take(&mut wrapped));
            count = 0;
            if c == '\n' {
                continue;
            }
        }
        wrapped.push(c);
        count += 1;
    }
    out
}
fn write_line(
    doc: &printpdf::PdfDocumentReference,
    font: &printpdf::IndirectFontRef,
    page: &mut printpdf::PdfPageIndex,
    layer: &mut printpdf::PdfLayerIndex,
    y: &mut f32,
    line: &str,
) {
    write_line_aligned(doc,font,page,layer,y,line,false);
}
fn write_line_aligned(
    doc: &printpdf::PdfDocumentReference, font: &printpdf::IndirectFontRef,
    page: &mut printpdf::PdfPageIndex, layer: &mut printpdf::PdfLayerIndex,
    y: &mut f32, line: &str, right: bool,
) {
    use ab_glyph::Font;
    let metrics=ab_glyph::FontRef::try_from_slice(include_bytes!("../assets/DejaVuSans.ttf")).expect("bundled font");
    for wrapped in wrap_report_line(line) {
        if *y < 18.0 {
            let next = doc.add_page(Mm(210.0), Mm(297.0), "Text");
            *page = next.0;
            *layer = next.1;
            *y = 280.0;
        }
        doc.get_page(*page).get_layer(*layer).use_text(
            &wrapped,
            if is_body_heading(line) { 11.0 } else { 10.0 },
            Mm(if right { (195.0-wrapped.chars().map(|c| metrics.h_advance_unscaled(metrics.glyph_id(c))).sum::<f32>()/metrics.units_per_em().unwrap_or(2048.0)*10.0*25.4/72.0).max(15.0) } else { 15.0 }),
            Mm(*y),
            font,
        );
        *y -= 5.0;
    }
}

pub fn render(
    detail: &StudyDetail,
    layout: &ReportLayout,
    photos: &[Photo],
    author: &str,
) -> Result<Rendered, String> {
    render_with_terms(detail, layout, photos, author, &[])
}

/// Native document preview uses the same block text as the PDF renderer.
pub fn native_lines_for_photos(
    detail: &StudyDetail,
    layout: &ReportLayout,
    author: &str,
    terms: &[m53_domain::validation::Term],
    selected: &[Uuid],
) -> Result<Vec<String>, String> {
    native_lines(
        &select_report_photos(detail, selected),
        layout,
        author,
        terms,
    )
}

pub fn native_lines(
    detail: &StudyDetail,
    layout: &ReportLayout,
    author: &str,
    terms: &[m53_domain::validation::Term],
) -> Result<Vec<String>, String> {
    let protocol = detail.protocol.as_ref().ok_or("Нет протокола")?;
    let labels = terms
        .iter()
        .map(|t| (t.code.clone(), t.label.clone()))
        .collect();
    let mut out = vec![
        layout.title.clone(),
    ];
    for a in &detail.annotations {
        if protocol.colpo.is_none()
            && let AnnotationLayer::Callout { full_name, .. } = &a.layer
        {
            out.push(format!("{} — {}", a.label, full_name));
        }
    }
    for block in &report_blocks(layout) {
        if !matches!(block, ReportBlock::Photos) {
            out.extend(lines(block, detail, protocol, author, &labels));
        }
    }
    Ok(out.iter().flat_map(|line| wrap_report_line(line)).collect())
}
