//! Server-defined stationery; clinical data are always escaped, never interpreted as HTML.
use super::{escape, *};

pub(super) const FOOTER_START: &str = "<template id='m53-print-footer'>";

fn date(value: &str) -> Option<(i32, u32, u32)> {
    let parts: Vec<_> = value.split('-').collect();
    if parts.len() != 3 { return None; }
    let (y,m,d)=(parts[0].parse().ok()?,parts[1].parse().ok()?,parts[2].parse().ok()?);
    let days=match m {1|3|5|7|8|10|12=>31,4|6|9|11=>30,2=>if y%4==0 && (y%100!=0 || y%400==0) {29}else{28},_=>return None};
    (y>=1 && d>=1 && d<=days).then_some((y,m,d))
}
fn date_label(value: &str) -> String {
    date(value).map(|(y,m,d)|format!("{d:02}.{m:02}.{y:04}")).unwrap_or_else(||"Не указана".into())
}
fn exam_date(detail: &StudyDetail) -> String {
    detail.protocol.as_ref().and_then(|p|p.colpo.as_ref())
        .and_then(|c|c.fields.get("context.exam_date")).map(|a|a.display()).unwrap_or_default()
}
fn patient_value(field: &str, detail: &StudyDetail) -> Result<String,String> {
    let patient=&detail.patient;
    Ok(match field {
        "full_name"=>[Some(patient.family_name.as_str()),Some(patient.given_name.as_str()),patient.patronymic.as_deref()]
            .into_iter().flatten().filter(|v|!v.trim().is_empty()).collect::<Vec<_>>().join(" "),
        // External GUID is not a medical-card number. Preserve leading zeroes from 1C.
        "card_number"=>patient.card_number.as_deref().filter(|s|!s.trim().is_empty()).unwrap_or("Не указана").into(),
        "sex"=>match patient.sex {Some(m53_contracts::colposcopy_1c::Sex::F)=>"Женский",Some(m53_contracts::colposcopy_1c::Sex::M)=>"Мужской",Some(_)=>"Другой",None=>"Не указан"}.into(),
        "birth_date"=>date_label(patient.birth_date.as_deref().unwrap_or("")),
        "exam_date"=>date_label(&exam_date(detail)),
        "age"=>patient.birth_date.as_deref().and_then(date).zip(date(&exam_date(detail)))
            .filter(|(birth,exam)|birth<=exam)
            .map(|((by,bm,bd),(ey,em,ed))|(ey-by-i32::from((em,ed)<(bm,bd))).to_string())
            .unwrap_or_else(||"Не указан".into()),
        _=>return Err(format!("Неизвестное поле шапки отчёта: {field}")),
    })
}

pub(super) fn header(detail: &StudyDetail, layout: &ReportLayout) -> Result<String,String> {
    let Some(header)=&layout.letterhead else {return Ok(String::new());};
    if header.clinic.is_none() {
        return Err("Филиал клиники не настроен для рабочего места. Обратитесь к администратору".into());
    }
    if header.patient_rows.len()>16 || header.logo_data_uri.len()>524288 {
        return Err("Слишком большой шаблон шапки отчёта".into());
    }
    let logo=header.logo_data_uri.trim();
    if !logo.is_empty() && !["data:image/svg+xml;base64,","data:image/png;base64,","data:image/jpeg;base64,"]
        .iter().any(|prefix|logo.strip_prefix(prefix).is_some_and(|data|STANDARD.decode(data).is_ok())) {
        return Err("Логотип должен быть встроенным изображением".into());
    }
    let mut out=String::from("<div class='letterhead'>");
    if !logo.is_empty() {out.push_str(&format!("<img alt='Логотип клиники' src='{}'>",escape(logo)));}
    out.push_str("<div class='letterhead-patient'>");
    for row in &header.patient_rows {
        out.push_str(&format!("<p><b>{}:</b> {}</p>",escape(row.label.trim_end_matches(':')),escape(&patient_value(&row.field,detail)?)));
    }
    out.push_str("</div></div>");
    Ok(out)
}

#[cfg(all(target_os = "linux", feature = "standalone"))]
pub(super) fn text_lines(detail: &StudyDetail, layout: &ReportLayout) -> Result<Vec<String>, String> {
    let Some(header) = &layout.letterhead else { return Ok(Vec::new()); };
    let clinic = header.clinic.as_ref().ok_or("Филиал клиники не настроен")?;
    let mut lines = vec![clinic.name.clone()];
    for (label, value) in [("Лицензия", &clinic.license), ("Телефон", &clinic.phone)] {
        if !value.trim().is_empty() {
            lines.push(format!("{label}: {value}"));
        }
    }
    for row in &header.patient_rows {
        lines.push(format!("{}: {}", row.label.trim_end_matches(':'), patient_value(&row.field, detail)?));
    }
    Ok(lines)
}

pub(super) fn footer(layout: &ReportLayout) -> String {
    let clinic=layout.letterhead.as_ref().and_then(|h|h.clinic.as_ref());
    let lines=clinic.map(|c|[c.name.clone(),if c.license.trim().is_empty(){String::new()}else{format!("Лицензия: {}",c.license)},if c.phone.trim().is_empty(){String::new()}else{format!("Телефон: {}",c.phone)}])
        .unwrap_or_default().into_iter().filter(|s|!s.trim().is_empty()).map(|s|escape(&s)).collect::<Vec<_>>().join("<br>");
    format!("{FOOTER_START}<div style='width:100%;padding:0 15mm;font:8px Arial,sans-serif;color:#444;display:flex;align-items:flex-end;gap:4mm'><div style='flex:1;overflow-wrap:anywhere'>{lines}</div><div style='white-space:nowrap'>Страница <span class='pageNumber'></span> из <span class='totalPages'></span></div></div></template>")
}

/// Read only the self-contained footer produced by this renderer, never document scripts.
#[cfg(windows)]
pub(super) fn print_footer(html: &str) -> Option<&str> {
    html.split_once(FOOTER_START)?.1.split_once("</template>").map(|(footer,_)|footer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_dates_reject_impossible_days() {
        assert!(date("2024-02-29").is_some());assert!(date("2025-02-29").is_none());
        assert!(date("2026-13-01").is_none());assert_eq!(date_label("2026-10-21"),"21.10.2026");
    }
}
