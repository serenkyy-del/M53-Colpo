//! Escaped, self-contained clinical report. No executable or remote content.
use super::*;
use std::collections::BTreeMap;

const CSS: &str = r#"
@page { size:A4; margin:15mm 15mm 24mm; }
* { box-sizing:border-box; }
body { margin:0; color:#111; background:white; font:10pt Arial,'Segoe UI',sans-serif; line-height:1.25; }
h1 { font-size:12pt; font-weight:400; margin:0 0 3mm; }
p { margin:0 0 1.5mm; white-space:pre-wrap; overflow-wrap:anywhere; }
.warning { margin-bottom:2mm; }
.letterhead { display:flex; align-items:flex-start; gap:5mm; margin-bottom:4mm; break-inside:avoid; }
.letterhead > img { width:65mm; height:auto; flex-shrink:0; }
.letterhead-patient { flex:1; min-width:0; font-size:10pt; }
.letterhead-patient p { margin:0 0 1mm; }
.body { margin-top:5mm; }
table { width:100%; border-collapse:collapse; table-layout:fixed; margin:0 0 4mm; break-inside:avoid; page-break-inside:avoid; }
th,td { border:0.6pt solid #333; padding:1.1mm 1.5mm; vertical-align:top; text-align:left; overflow-wrap:anywhere; }
thead { display:table-header-group; }
thead th { background:#d8d8d8; text-align:center; font-weight:400; }
th { font-weight:700; background:#f2f2f2; }
/* Chromium may fragment an oversized table, but a fitting row stays intact. */
tr { break-inside:avoid; page-break-inside:avoid; }
.entry { margin:0 0 0.6mm; white-space:pre-wrap; }
.entry:last-child { margin-bottom:0; }
b { font-weight:700; }
.ref { font-size:9pt; }
.context { margin-bottom:4mm; }
.context p { display:inline; margin-right:4mm; }
.conclusion { break-inside:avoid; page-break-inside:avoid; }
.conclusion tr, .conclusion td { break-inside:avoid; page-break-inside:avoid; }
.conclusion td { min-height:16mm; }
.conclusion .entry { orphans:3; widows:3; }
.signature { margin-top:6mm; }
.photo-page { break-before:page; page-break-before:always; }
.photo-page h2 { font-size:11pt; font-weight:400; margin:0 0 3mm; }
.photo-page img { display:block; width:100%; height:170mm; object-fit:contain; }
.photo-legend { margin-top:3mm; }
.photo-legend h3 { font-size:10pt; margin:0 0 2mm; break-after:avoid; }
.photo-legend .entry { break-inside:avoid; }
@media screen { body { max-width:180mm; margin:15mm auto; } }
"#;

fn rich(text: &str) -> String {
    if let Some((label, rest)) = text.split_once(':') {
        format!("<b>{}:</b>{}", escape(label), escape(rest))
    } else {
        escape(text)
    }
}

fn entries(rows: &[String]) -> String {
    entries_styled(rows, false)
}

fn entries_styled(rows: &[String], full_first: bool) -> String {
    // Merge only identical designations WITH identical properties. References remain explicit.
    let mut grouped: Vec<(String, String, BTreeMap<usize, Vec<String>>, Vec<String>)> = vec![];
    let mut result = String::new();
    for row in rows {
        let Some((name, reference)) = row.split_once(". Снимок ") else {
            result.push_str(&format!("<div class='entry'>{}</div>", rich(row)));
            continue;
        };
        let (reference, properties) = reference.split_once("; ").unwrap_or((reference, ""));
        let Some((photo, mark)) = reference.split_once(", метка ") else {
            result.push_str(&format!("<div class='entry'>{}</div>", rich(row)));
            continue;
        };
        let Ok(photo) = photo.parse::<usize>() else {
            result.push_str(&format!("<div class='entry'>{}</div>", rich(row)));
            continue;
        };
        let index = grouped
            .iter()
            .position(|g| g.0 == name && g.1 == properties)
            .unwrap_or_else(|| {
                grouped.push((name.into(), properties.into(), BTreeMap::new(), vec![]));
                grouped.len() - 1
            });
        grouped[index].2.entry(photo).or_default().push(mark.into());
        grouped[index].3.push(row.clone());
    }
    for (name, properties, photos, originals) in grouped {
        let name = if let Some((short, full)) = name.split_once(" — ") {
            if full_first {
                format!("{} (<b>{}</b>)", escape(full), escape(short))
            } else {
                format!("<b>{}</b> — {}", escape(short), escape(full))
            }
        } else {
            format!("<b>{}</b>", escape(&name))
        };
        let refs = photos
            .into_iter()
            .map(|(photo, ids)| format!("сн. {photo}: {}", ids.join(", ")))
            .collect::<Vec<_>>()
            .join("; ");
        let props = if properties.is_empty() {
            String::new()
        } else {
            format!("; {}", escape(&properties))
        };
        if full_first {
            result.push_str(&format!("<div class='entry'>{name}{props}</div>"));
        } else {
            result.push_str(&format!(
                "<div class='entry' title='{}'>{name}{props} <span class='ref'>({})</span></div>",
                escape(&originals.join(" / ")),
                escape(&refs)
            ));
        }
    }
    result
}

fn vulva_table(title: &str, rows: &[String]) -> String {
    let mut sections: Vec<(Option<&str>, Vec<String>)> = vec![(None, vec![])];
    for row in rows {
        if matches!(
            row.as_str(),
            "Нормальные картины:"
                | "Тип поражения и цвет кожи:"
                | "Вторичная морфология:"
                | "Различные находки:"
                | "Подозрение на рак:"
        ) {
            sections.push((Some(row.trim_end_matches(':')), vec![]));
        } else {
            sections.last_mut().unwrap().1.push(row.clone());
        }
    }
    let body = sections
        .into_iter()
        .filter(|(_, items)| !items.is_empty())
        .map(|(heading, items)| {
            let heading = heading
                .map(|h| format!("<div class='entry'><b>{}:</b></div>", escape(h)))
                .unwrap_or_default();
            format!("<tr><td>{heading}{}</td></tr>", entries(&items))
        })
        .collect::<String>();
    format!(
        "<table class='section vulva'><thead><tr><th>{}</th></tr></thead><tbody>{body}</tbody></table>",
        escape(title)
    )
}

fn table(title: &str, rows: &[String]) -> String {
    if title.starts_with("Оценка вульвы") {
        return vulva_table(title, rows);
    }

    let contents = if title == "Кольпоскопическое заключение" {
        rows.iter()
            .map(|text| format!("<div class='entry'>{}</div>", escape(text)))
            .collect()
    } else {
        entries(rows)
    };
    format!(
        "<table class='{}'><thead><tr><th>{}</th></tr></thead><tbody><tr><td>{}</td></tr></tbody></table>",
        if title == "Кольпоскопическое заключение" {
            "conclusion"
        } else {
            "section"
        },
        escape(title),
        contents
    )
}

fn body(detail: &StudyDetail, p: &Protocol, assess: bool, findings: bool) -> String {
    let c = p.colpo.as_ref().unwrap();
    let mut rows = if assess {
        colpo_body::assessment(detail, p, c)
    } else {
        vec![]
    };
    if findings {
        rows.extend(colpo_body::findings(detail, p, c));
    }
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut heading = String::new();
    for row in rows {
        if is_body_heading(&row) {
            heading = row;
        } else {
            groups.entry(heading.clone()).or_default().push(row);
        }
    }
    let take = |i: usize| {
        groups
            .get(colpo_body::HEADINGS[i])
            .cloned()
            .unwrap_or_default()
    };
    let mut out = String::from("<div class='body'>");
    if assess {
        out.push_str("<div class='context'>");
        for row in take(0) {
            out.push_str(&format!("<p>{}</p> ", rich(&row)));
        }
        out.push_str("</div>");
        if !c.hidden() {
            if !take(1).is_empty() {
                let mut cells: [Vec<String>; 3] = Default::default();
                for row in take(1) {
                    let i = if row.starts_with("Форма:") || row == "Гипертрофия" {
                        0
                    } else if row.starts_with("Поверхность") || row.starts_with("Цвет")
                    {
                        2
                    } else {
                        1
                    };
                    let row = if (i == 0 && row.starts_with("Форма:"))
                        || (i == 1 && row.starts_with("Зев:"))
                    {
                        row.split_once(':').unwrap().1.trim().to_owned()
                    } else {
                        row
                    };
                    cells[i].push(row);
                }
                out.push_str(
                    "<table><thead><tr><th colspan='3'>Шейка матки</th></tr></thead><tbody><tr>",
                );
                for (i, label) in ["Форма", "Зев", "Слизистая"].iter().enumerate()
                {
                    out.push_str(&format!(
                        "<td><b>{label}:</b><br>{}</td>",
                        entries(&cells[i])
                    ));
                }
                out.push_str("</tr></tbody></table>");
            }
            if !take(2).is_empty() {
                out.push_str(&table("Простая кольпоскопия", &take(2)));
            }
        }
    }
    if !c.hidden() && (assess || findings) && (3..=5).any(|i| !take(i).is_empty()) {
        out.push_str("<table><thead><tr><th colspan='2'>Кольпоскопия</th></tr></thead><tbody>");
        if !take(3).is_empty() {
            out.push_str(&format!(
                "<tr><td colspan='2'>{}</td></tr>",
                entries(&take(3))
            ));
        }
        if findings && (!take(4).is_empty() || !take(5).is_empty()) {
            out.push_str("<tr><th>Нормальные кольпоскопические картины</th><th>Аномальные кольпоскопические картины</th></tr>");
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td></tr>",
                entries(&take(4)),
                entries(&take(5))
            ));
        }
        out.push_str("</tbody></table>");
    }
    if findings {
        for i in 6..=10 {
            let rows = take(i);
            if !rows.is_empty() {
                out.push_str(&table(colpo_body::HEADINGS[i], &rows));
            }
        }
    }
    out.push_str("</div>");
    out
}

pub fn render(
    detail: &StudyDetail,
    layout: &ReportLayout,
    photos: &[Photo],
    author: &str,
    labels: &BTreeMap<String, String>,
) -> Result<Vec<u8>, String> {
    let p = detail.protocol.as_ref().ok_or("Нет протокола")?;
    let mut out = format!(
        "<!doctype html><html lang='ru'><head><meta charset='utf-8'><meta http-equiv='Content-Security-Policy' content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'\"><title>{}</title><style>{CSS}</style></head><body>{}<h1>{}</h1>",
        escape(&layout.title),
        letterhead::header(detail,layout)?,
        escape(&layout.title)
    );
    let mut done = false;
    for block in &report_blocks(layout) {
        match block {
            ReportBlock::Patient if layout.letterhead.is_some() => (),
            ReportBlock::History => {
                if let Assessment::Assessed(value) = &p.history.value {
                    if !value.trim().is_empty() { out.push_str(&table("Анамнез", &[value.clone()])); }
                }
            }
            ReportBlock::Photos => (),
            ReportBlock::Assessment | ReportBlock::Findings => {
                if !done {
                    out.push_str(&body(
                        detail,
                        p,
                        layout
                            .blocks
                            .iter()
                            .any(|b| matches!(b, ReportBlock::Assessment)),
                        layout
                            .blocks
                            .iter()
                            .any(|b| matches!(b, ReportBlock::Findings)),
                    ));
                    done = true;
                }
            }
            ReportBlock::Conclusion => {
                if let Some(rows) = p.diagnoses.as_ref().filter(|rows| !rows.is_empty()) {
                    out.push_str("<table><thead><tr><th>Диагнозы</th></tr></thead><tbody>");
                    for row in rows {
                        let mut entry = format!("{} {}", row.code, row.name);
                        if !row.additional_text.is_empty() {
                            entry.push('\n');
                            entry.push_str(&row.additional_text);
                        }
                        out.push_str(&format!("<tr><td><div class='entry'>{}</div></td></tr>", escape(&entry)));
                    }
                    out.push_str("</tbody></table>");
                }
                if let Some(note) = p.diagnosis_note.as_ref()
                    && let Assessment::Assessed(value) = &note.value
                    && !value.trim().is_empty()
                {
                    out.push_str(&table("Дополнение к диагнозам", &[value.clone()]));
                }
                let value = match &p.conclusion.value {
                    Assessment::Assessed(v) => v.clone(),
                    _ => String::new(),
                };
                out.push_str(&table("Кольпоскопическое заключение", &[value]));
            }
            ReportBlock::Signature => {
                for line in lines(block, detail, p, author, labels) {
                    out.push_str(&format!("<p class='physician-signature' style='text-align:right'>{}</p>",escape(&line)));
                }
            }
            _ => {
                for line in lines(block, detail, p, author, labels) {
                    out.push_str(&format!("<p>{}</p>", escape(&line)));
                }
            }
        }
    }
    if layout
        .blocks
        .iter()
        .any(|b| matches!(b, ReportBlock::Photos))
    {
        for photo in photos {
            let decoded = crate::imaging::decode(&photo.bytes)?;
            let annotated =
                crate::imaging::annotated(&decoded, &annotations_for_report(detail, photo.id));
            let encoded = STANDARD.encode(crate::imaging::png(&annotated)?);
            let number = detail
                .assets
                .iter()
                .filter(|a| a.kind == "original")
                .position(|a| a.id == photo.id)
                .map(|i| i + 1)
                .unwrap_or(0);
            let rows = colpo_body::photo_findings(detail, photo.id);
            let legend = if rows.is_empty() {
                String::new()
            } else {
                format!(
                    "<div class='photo-legend'><h3>Описание кольпоскопической картины</h3>{}</div>",
                    entries_styled(&rows, true)
                )
            };
            out.push_str(&format!("<section class='photo-page'><h2>Снимок {number}</h2><img alt='Снимок {number}' src='data:image/png;base64,{encoded}'>{legend}</section>"));
        }
    }
    out.push_str(&letterhead::footer(layout));
    out.push_str("</body></html>");
    Ok(out.into_bytes())
}

#[cfg(test)]
mod anamnesis_tests {
    use super::*;
    #[test]
    fn optional_history_is_printed_once_escaped_in_legacy_layouts() {
        let id = uuid::Uuid::new_v4();
        let mut detail = StudyDetail {
            mis_task_bound: false,
            study: Study { id, patient_id:id, status:"draft".into(), row_version:1, study_instance_uid:"2.25.1".into() },
            patient: Patient { id, external_id:None, family_name:"Тест".into(), given_name:"Тест".into(), patronymic:None, birth_date:None, sex:None, card_number:None },
            revision_id:Some(id), protocol:Some(crate::model::blank_protocol(id)), assets:vec![], annotations:vec![],
        };
        detail.protocol.as_mut().unwrap().colpo=Some(Default::default());
        for blocks in [vec![ReportBlock::Assessment, ReportBlock::Conclusion], vec![ReportBlock::History,ReportBlock::History,ReportBlock::Assessment]] {
            let layout = ReportLayout { title:"Тест".into(), photo_columns:3, letterhead:None, blocks };
            for text in ["", "  ", "Первая строка\n<script>тест</script> & вторая"] {
                detail.protocol.as_mut().unwrap().history.value=Assessment::Assessed(text.into());
                let output=String::from_utf8(render(&detail,&layout,&[],"Врач",&BTreeMap::new()).unwrap()).unwrap();
                assert_eq!(output.matches("<th>Анамнез</th>").count(), usize::from(!text.trim().is_empty()));
                if !text.trim().is_empty() {
                    assert!(output.contains("Первая строка\n&lt;script&gt;тест&lt;/script&gt; &amp; вторая"));
                    assert!(!output.contains("<script>"));
                    assert!(output.find("<th>Анамнез</th>").unwrap()<output.find("Общая оценка").unwrap_or(output.len()));
                }
            }
            detail.protocol.as_mut().unwrap().history.value=Assessment::NotAssessed;
            assert!(!String::from_utf8(render(&detail,&layout,&[],"Врач",&BTreeMap::new()).unwrap()).unwrap().contains("<th>Анамнез</th>"));
        }
    }
}

#[cfg(all(test, windows))]
mod pagination_tests {
    use super::*;

    #[test]
    #[ignore = "Run scripts/test-report-pagination.py for Chromium and independent PDF assertions"]
    fn tables_move_together_and_oversized_tables_keep_all_content() {
        let short = vulva_table("VULVA_SHORT", &[
            "Кольпоскопическая картина: адекватная".into(),
            "Тип поражения и цвет кожи:".into(), "SHORT_SKIN".into(),
            "Вторичная морфология:".into(), "SHORT_MORPHOLOGY".into(),
            "Различные находки:".into(), "SHORT_FINDING".into(),
        ]);
        let mut long_rows = vec![];
        for (heading, prefix) in [("Тип поражения и цвет кожи:", "SKIN"), ("Вторичная морфология:", "MORPH"), ("Различные находки:", "OTHER")] {
            long_rows.push(heading.into());
            long_rows.extend((0..32).map(|i| format!("{prefix}_{i:03}")));
        }
        let oversized = (0..150).map(|i| format!("LINE_{i:03}")).collect::<Vec<_>>().join("\n");
        for (name, contents, tokens) in [
            ("short", format!("<div style='height:220mm'></div>{short}"), vec!["VULVA_SHORT".to_string(), "SHORT_SKIN".into(), "SHORT_MORPHOLOGY".into(), "SHORT_FINDING".into()]),
            ("long", vulva_table("VULVA_LONG", &long_rows), (0..32).flat_map(|i| [format!("SKIN_{i:03}"), format!("MORPH_{i:03}"), format!("OTHER_{i:03}")]).collect()),
            ("oversized", table("Кольпоскопическое заключение", &[oversized]), (0..150).map(|i| format!("LINE_{i:03}")).collect()),
        ] {
            let html = format!("<!doctype html><html><head><meta charset='utf-8'><style>{CSS}</style></head><body>{contents}</body></html>");
            let pdf = chromium::pdf(html.as_bytes()).unwrap();
            if let Some(dir) = std::env::var_os("M53_PAGINATION_PROOF_DIR") {
                let dir = std::path::PathBuf::from(dir);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(format!("{name}.pdf")), &pdf).unwrap();
                std::fs::write(dir.join(format!("{name}.json")), serde_json::to_vec(&tokens).unwrap()).unwrap();
                for (i, png) in chromium::pages(&pdf).unwrap().iter().enumerate() {
                    std::fs::write(dir.join(format!("{name}-{}.png", i + 1)), png).unwrap();
                }
            }
        }
    }
}
