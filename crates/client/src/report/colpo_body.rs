//! Clinical body shared by the native preview and client PDF. Patient/header blocks stay separate.
use m53_contracts::workflow::StudyDetail;
use m53_domain::{
    colpo::{Answer, Colpo, FIELDS, PRESETS, Preset},
    protocol::*,
};
use uuid::Uuid;

pub const HEADINGS: &[&str] = &[
    "Клинический контекст",
    "Шейка матки",
    "Простая кольпоскопия",
    "Кольпоскопия · общие положения",
    "Нормальные кольпоскопические картины",
    "Аномальные кольпоскопические картины",
    "Подозрение на инвазию",
    "Другие кольпоскопические картины",
    "Ректо-вагинальный осмотр",
    "Оценка вульвы · клиническая терминология",
    "Оценка вульвы · кольпоскопическая терминология",
];

fn display(value: &Answer) -> Option<String> {
    match value {
        Answer::Checked(false) => None,
        Answer::Checked(true) => Some("Да".into()),
        Answer::Text(s) | Answer::Choice(s) if s.trim().is_empty() => None,
        Answer::Text(s) | Answer::Choice(s) => Some(match s.as_str() {
            "БледноРозовая" => "Бледно-розовая".into(),
            "НеВидна" => "Не видна".into(),
            _ => s.trim().into(),
        }),
    }
}

fn fields(colpo: &Colpo, ids: &[&str]) -> Vec<String> {
    ids.iter()
        .filter_map(|id| {
            let field = FIELDS.iter().find(|f| f.id == *id)?;
            let value = colpo.fields.get(*id)?;
            if matches!(value, Answer::Checked(true)) {
                return Some(field.label.into());
            }
            let mut text = display(value)?;
            if matches!(field.control, m53_domain::colpo::Control::Date) {
                let parts: Vec<_> = text.split('-').collect();
                if parts.len() == 3 {
                    text = format!("{}.{}.{}", parts[2], parts[1], parts[0]);
                }
            }
            Some(format!("{}: {text}", field.label))
        })
        .collect()
}

fn section(out: &mut Vec<String>, title: &str, rows: Vec<String>) {
    if !rows.is_empty() {
        out.push(title.into());
        out.extend(rows);
    }
}

/// The same per-photo, per-designation ordinal is used by the editor and report.
pub fn mark_number(colpo: &Colpo, id: Uuid) -> Option<usize> {
    let mark = colpo.marks.get(&id)?;
    colpo
        .marks
        .iter()
        .filter(|(_, m)| m.asset_id == mark.asset_id && m.catalog_id == mark.catalog_id)
        .position(|(candidate, _)| *candidate == id)
        .map(|i| i + 1)
}

fn marks(
    detail: &StudyDetail,
    colpo: &Colpo,
    presets: impl Iterator<Item = &'static Preset>,
) -> Vec<String> {
    let mut rows = vec![];
    for preset in presets {
        for (id, mark) in colpo
            .marks
            .iter()
            .filter(|(_, m)| m.catalog_id == preset.id)
        {
            let photo = detail
                .assets
                .iter()
                .filter(|a| a.kind == "original")
                .position(|a| a.id == mark.asset_id)
                .map(|i| (i + 1).to_string())
                .unwrap_or_else(|| "—".into());
            let properties: Vec<_> = FIELDS
                .iter()
                .filter(|f| f.parent == preset.id)
                .filter_map(|f| {
                    let value = mark.properties.get(f.id)?;
                    let text = display(value)?;
                    Some(if matches!(value, Answer::Checked(true)) {
                        f.label.into()
                    } else {
                        format!("{}: {text}", f.label)
                    })
                })
                .collect();
            let suffix = if properties.is_empty() {
                String::new()
            } else {
                format!("; {}", properties.join(", "))
            };
            rows.push(format!(
                "{} — {}. Снимок {photo}, метка {}{suffix}",
                preset.short,
                preset.name,
                mark_number(colpo, *id).unwrap_or(1)
            ));
        }
    }
    rows
}

fn adequacy(p: &Protocol, region: Region) -> Vec<String> {
    p.sections
        .iter()
        .filter(|s| s.region == region)
        .filter_map(|s| {
            if let Assessment::Assessed(value) = &s.adequacy.value {
                Some(match value {
                    Adequacy::Adequate => "Кольпоскопическая картина: адекватная".into(),
                    Adequacy::Inadequate { reason } => {
                        format!("Кольпоскопическая картина: неадекватная; причина: {reason}")
                    }
                })
            } else {
                None
            }
        })
        .collect()
}

pub fn assessment(detail: &StudyDetail, p: &Protocol, c: &Colpo) -> Vec<String> {
    let mut out = vec![];
    let mut context = fields(
        c,
        &[
            "context.exam_date",
            "context.lmp",
            "context.phase",
            "context.pregnancy",
            "context.menopause",
            "context.no_cervix",
            "context.skip",
        ],
    );
    if let Some(day) = c.cycle_day() {
        context.push(format!("День цикла: {day}"));
    }
    section(&mut out, HEADINGS[0], context);
    if !c.hidden() {
        let mut cervix = fields(c, &["cervix.shape", "cervix.hypertrophy", "cervix.os"]);
        cervix.extend(marks(
            detail,
            c,
            PRESETS
                .iter()
                .filter(|p| p.region == Region::Cervix && p.group == 0),
        ));
        cervix.extend(fields(c, &["cervix.surface", "cervix.color"]));
        section(&mut out, HEADINGS[1], cervix);
        section(
            &mut out,
            HEADINGS[2],
            fields(
                c,
                &[
                    "simple.relief",
                    "simple.vessels",
                    "simple.closed",
                    "simple.closed_count",
                    "simple.open",
                    "simple.open_count",
                    "simple.transition",
                ],
            ),
        );
        let mut general = adequacy(p, Region::Cervix);
        match p.cervical_assessment.transformation_zone.value {
            Assessment::Assessed(tz) => general.push(format!(
                "Зона трансформации: {}",
                match tz {
                    CervicalTransformationZone::Type1 => "ЗТ I",
                    CervicalTransformationZone::Type2 => "ЗТ II",
                    CervicalTransformationZone::Type3 => "ЗТ III",
                }
            )),
            Assessment::NotApplicable => general.push("Зона трансформации: Не применимо".into()),
            Assessment::NotAssessed => (),
        }
        section(&mut out, HEADINGS[3], general);
    }
    out
}

pub fn findings(detail: &StudyDetail, p: &Protocol, c: &Colpo) -> Vec<String> {
    let mut out = vec![];
    if !c.hidden() {
        section(
            &mut out,
            HEADINGS[4],
            marks(
                detail,
                c,
                PRESETS.iter().filter(|p| {
                    p.region == Region::Cervix
                        && p.group == 2
                        && (m53_domain::colpo::subgroup(p).1 == 0 || p.id == "cervix.symbol.6")
                }),
            ),
        );
        let mut abnormal = fields(c, &["extended.location", "extended.clock", "extended.size"]);
        abnormal.extend(marks(
            detail,
            c,
            PRESETS.iter().filter(|p| {
                p.region == Region::Cervix
                    && p.group == 2
                    && p.id != "cervix.symbol.6"
                    && matches!(m53_domain::colpo::subgroup(p).1, 1..=3)
            }),
        ));
        section(&mut out, HEADINGS[5], abnormal);
        section(
            &mut out,
            HEADINGS[6],
            marks(
                detail,
                c,
                PRESETS
                    .iter()
                    .filter(|p| p.region == Region::Cervix && p.id == "cervix.symbol.12"),
            ),
        );
        let mut other = marks(
            detail,
            c,
            PRESETS.iter().filter(|p| {
                p.region == Region::Cervix && p.group == 2 && m53_domain::colpo::subgroup(p).1 == 5
            }),
        );
        other.extend(fields(c, &["extended.other"]));
        section(&mut out, HEADINGS[7], other);
        section(
            &mut out,
            HEADINGS[8],
            fields(c, &["rectovaginal.description"]),
        );
    }
    let mut vulva = adequacy(p, Region::Vulva);
    for group in 0..=4 {
        let mut rows = marks(
            detail,
            c,
            PRESETS.iter().filter(|p| {
                p.region == Region::Vulva
                    && p.group == 0
                    && m53_domain::colpo::subgroup(p).1 == group
            }),
        );
        if group == 1 {
            rows.extend(fields(
                c,
                &["vulva.skin.red", "vulva.skin.white", "vulva.skin.dark"],
            ));
        }
        if group == 3 {
            rows.extend(fields(c, &["vulva.other_comment"]));
        }
        if group == 4 {
            rows.extend(fields(c, &["vulva.cancer_comment"]));
        }
        if !rows.is_empty() {
            let title = [
                "Нормальные картины",
                "Тип поражения и цвет кожи",
                "Вторичная морфология",
                "Различные находки",
                "Подозрение на рак",
            ][group];
            vulva.push(format!("{title}:"));
            vulva.extend(rows);
        }
    }
    section(&mut out, HEADINGS[9], vulva);
    let mut magnification = marks(
        detail,
        c,
        PRESETS
            .iter()
            .filter(|p| p.region != Region::Cervix && p.group == 1),
    );
    magnification.extend(adequacy(p, Region::Anus));
    section(&mut out, HEADINGS[10], magnification);
    out
}

/// Photo-local legend, retaining each marker's own properties and ordinal.
pub fn photo_findings(detail: &StudyDetail, asset: Uuid) -> Vec<String> {
    let Some(c) = detail.protocol.as_ref().and_then(|p| p.colpo.as_ref()) else {
        return vec![];
    };
    let mut c = c.clone();
    c.marks.retain(|_, m| m.asset_id == asset);
    marks(detail, &c, PRESETS.iter())
}
