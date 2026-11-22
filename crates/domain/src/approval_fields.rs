//! User-confirmed manual fields required at approval, never draft defaults.
use crate::{
    colpo::{Answer, FIELDS, validate_value},
    protocol::{Assessment, Protocol, Region},
    validation::ValidationError,
};

pub const REQUIRED_CERVIX_FIELDS: &[&str] = &[
    "cervix.shape",
    "cervix.os",
    "cervix.surface",
    "cervix.color",
];
pub const REQUIRED_SIMPLE_FIELDS: &[&str] =
    &["simple.relief", "simple.vessels", "simple.transition"];
/// Explicit existing UI group "Аномальные кольпоскопические картины".
/// No diagnosis is inferred from iodine, normal marks, or other regions.
pub const ANOMALOUS_CERVIX_PRESETS: &[&str] = &["cervix.symbol.7", "cervix.symbol.8"];
fn error(errors: &mut Vec<ValidationError>, path: String) {
    errors.push(ValidationError {
        path,
        code: "required_colpo_field".into(),
    });
}
/// Caller invokes this only in ValidationMode::Approval. No mutation or IO.
pub fn validate(protocol: &Protocol) -> Vec<ValidationError> {
    if protocol.schema_version != 2 {
        return vec![];
    }
    let Some(colpo) = &protocol.colpo else {
        return vec![];
    };
    if colpo.hidden() {
        return vec![];
    }
    let mut required = REQUIRED_CERVIX_FIELDS
        .iter()
        .chain(REQUIRED_SIMPLE_FIELDS.iter())
        .copied()
        .collect::<Vec<_>>();
    for (flag, count) in [
        ("simple.closed", "simple.closed_count"),
        ("simple.open", "simple.open_count"),
    ] {
        if colpo.fields.get(flag).is_some_and(Answer::checked) {
            required.push(count);
        }
    }
    if colpo
        .marks
        .values()
        .any(|mark| ANOMALOUS_CERVIX_PRESETS.contains(&mark.catalog_id.as_str()))
        || ANOMALOUS_CERVIX_PRESETS
            .iter()
            .any(|id| colpo.fields.get(*id).is_some_and(Answer::checked))
    {
        required.extend(["extended.location", "extended.size"]);
    }
    let mut errors = vec![];
    for id in required {
        let valid = colpo.fields.get(id).is_some_and(|answer| {
            matches!(answer, Answer::Choice(_))
                && FIELDS
                    .iter()
                    .find(|f| f.id == id && f.parent.is_empty())
                    .is_some_and(|f| validate_value(f, answer))
        });
        if !valid {
            error(&mut errors, format!("/colpo/fields/{id}"));
        }
    }
    if let Some((index, section)) = protocol
        .sections
        .iter()
        .enumerate()
        .find(|(_, s)| s.region == Region::Cervix)
        && !matches!(section.adequacy.value, Assessment::Assessed(_))
    {
        errors.push(ValidationError {
            path: format!("/sections/{index}/adequacy"),
            code: "adequacy_required".into(),
        });
    }
    if !matches!(
        protocol.cervical_assessment.transformation_zone.value,
        Assessment::Assessed(_) | Assessment::NotApplicable
    ) {
        errors.push(ValidationError {
            path: "/cervical_assessment/transformation_zone".into(),
            code: "transformation_zone_required".into(),
        });
    }
    errors
}
