//! Portable, physician-entered protocol values; never a patient or image snapshot.
use crate::{
    colpo::{self, Answer, Colpo},
    protocol::{Adequacy, Assessment, CervicalTransformationZone, Field, Origin, Protocol, ProtocolDiagnosis, Region},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortableDiagnosis {
    pub diagnosis_id: Uuid,
    pub code: String,
    pub name: String,
    pub additional_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegionAdequacy {
    pub region: Region,
    pub value: Assessment<Adequacy>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisitTemplate {
    pub format_version: u32,
    pub colpo_fields: BTreeMap<String, Answer>,
    pub history: Option<Assessment<String>>,
    pub conclusion: Option<Assessment<String>>,
    pub diagnosis_note: Option<Assessment<String>>,
    pub diagnoses: Vec<PortableDiagnosis>,
    pub adequacy: Vec<RegionAdequacy>,
    pub transformation_zone: Option<Assessment<CervicalTransformationZone>>,
}

fn allowed_field(id: &str) -> bool {
    if matches!(id, "context.exam_date" | "context.lmp" | "extended.adequacy" | "extended.tz") { return false; }
    matches!(id, "cervix.symbol.7" | "cervix.symbol.8") || colpo::FIELDS.iter().any(|field| field.parent.is_empty() && field.id == id)
}
fn portable_field(id: &str, answer: &Answer) -> bool {
    if !allowed_field(id) { return false; }
    if matches!(id, "cervix.symbol.7" | "cervix.symbol.8") { return matches!(answer, Answer::Checked(_)); }
    colpo::FIELDS.iter().any(|field| field.parent.is_empty() && field.id == id && colpo::validate_value(field, answer))
}
fn physician<T: Clone>(field: &Field<T>) -> Option<T> {
    matches!(field.origin, Origin::Physician).then(|| field.value.clone())
}
fn valid_text(value: &Option<Assessment<String>>) -> bool {
    match value { Some(Assessment::Assessed(text)) => text.len() <= 16_384 && !text.contains('\0'), _ => true }
}
impl VisitTemplate {
    pub fn capture(protocol: &Protocol) -> Result<Self, String> {
        if protocol.colpo.as_ref().is_some_and(|c| c.fields.iter().any(|(id, value)| allowed_field(id) && !portable_field(id, value))) {
            return Err("Проверьте заполненные поля перед сохранением шаблона".into());
        }
        let fields = protocol.colpo.as_ref().map(|c| c.fields.iter().filter(|(id, v)| portable_field(id, v)).map(|(id,v)| (id.clone(),v.clone())).collect()).unwrap_or_default();
        let template = Self {
            format_version: 1,
            colpo_fields: fields,
            history: physician(&protocol.history),
            conclusion: physician(&protocol.conclusion),
            diagnosis_note: protocol.diagnosis_note.as_ref().and_then(physician).or_else(|| protocol.diagnosis_note.is_none().then_some(Assessment::NotAssessed)),
            diagnoses: protocol.diagnoses.as_ref().into_iter().flatten().map(|row| PortableDiagnosis {
                diagnosis_id: row.diagnosis_id, code: row.code.clone(), name: row.name.clone(), additional_text: row.additional_text.clone(),
            }).collect(),
            adequacy: protocol.sections.iter().filter_map(|section| physician(&section.adequacy).map(|value| RegionAdequacy { region: section.region, value })).collect(),
            transformation_zone: physician(&protocol.cervical_assessment.transformation_zone),
        };
        template.validate()?;
        Ok(template)
    }

    pub fn validate(&self) -> Result<(), String> {
        use crate::protocol::{DIAGNOSES_MAX_ROWS, DIAGNOSIS_CODE_MAX_BYTES, DIAGNOSIS_NAME_MAX_BYTES, DIAGNOSIS_ADDITIONAL_MAX_BYTES};
        if self.format_version != 1 || self.colpo_fields.iter().any(|(id,v)| !portable_field(id,v))
            || !valid_text(&self.history) || !valid_text(&self.conclusion) || !valid_text(&self.diagnosis_note)
            || self.diagnoses.len() > DIAGNOSES_MAX_ROWS
            || self.diagnoses.iter().any(|d| d.diagnosis_id.is_nil() || d.code.trim().is_empty() || d.name.trim().is_empty()
                || d.code.len() > DIAGNOSIS_CODE_MAX_BYTES || d.name.len() > DIAGNOSIS_NAME_MAX_BYTES || d.additional_text.len() > DIAGNOSIS_ADDITIONAL_MAX_BYTES
                || d.code.contains('\0') || d.name.contains('\0') || d.additional_text.contains('\0'))
            || self.adequacy.len() > 4 || self.adequacy.iter().enumerate().any(|(i,x)| self.adequacy[..i].iter().any(|y| y.region == x.region))
            || self.adequacy.iter().any(|a| matches!(&a.value, Assessment::Assessed(Adequacy::Inadequate { reason }) if reason.len() > 16_384 || reason.contains('\0')))
        { return Err("Некорректный шаблон приёма".into()); }
        Ok(())
    }

    pub fn apply(&self, protocol: &mut Protocol) -> Result<(), String> {
        self.validate()?;
        let mut result = protocol.clone();
        let colpo = result.colpo.get_or_insert_with(Colpo::default);
        colpo.fields.retain(|id,_| !allowed_field(id));
        colpo.fields.extend(self.colpo_fields.clone());
        if let Some(value) = &self.history { if matches!(result.history.origin, Origin::Physician) { result.history.value = value.clone(); } }
        if let Some(value) = &self.conclusion { if matches!(result.conclusion.origin, Origin::Physician) { result.conclusion.value = value.clone(); } }
        if let Some(value) = &self.diagnosis_note {
            if result.diagnosis_note.as_ref().is_none_or(|v| matches!(v.origin, Origin::Physician)) {
                result.diagnosis_note = Some(Field { value: value.clone(), origin: Origin::Physician });
            }
        }
        result.diagnoses = Some(self.diagnoses.iter().map(|d| ProtocolDiagnosis { id:Uuid::new_v4(), diagnosis_id:d.diagnosis_id, code:d.code.clone(), name:d.name.clone(), additional_text:d.additional_text.clone() }).collect());
        for entry in &self.adequacy {
            if let Some(section) = result.sections.iter_mut().find(|s| s.region == entry.region) {
                if matches!(section.adequacy.origin, Origin::Physician) { section.adequacy.value = entry.value.clone(); }
            }
        }
        if let Some(value) = &self.transformation_zone { if matches!(result.cervical_assessment.transformation_zone.origin, Origin::Physician) { result.cervical_assessment.transformation_zone.value = value.clone(); } }
        *protocol = result;
        Ok(())
    }

    pub fn clear(protocol: &mut Protocol) {
        if let Some(colpo) = &mut protocol.colpo { colpo.fields.retain(|id,_| !allowed_field(id)); }
        if matches!(protocol.history.origin, Origin::Physician) { protocol.history.value = Assessment::NotAssessed; }
        if matches!(protocol.conclusion.origin, Origin::Physician) { protocol.conclusion.value = Assessment::NotAssessed; }
        if protocol.diagnosis_note.as_ref().is_none_or(|v| matches!(v.origin, Origin::Physician)) {
            protocol.diagnosis_note = Some(Field { value: Assessment::NotAssessed, origin: Origin::Physician });
        }
        protocol.diagnoses = Some(vec![]);
        for section in &mut protocol.sections { if matches!(section.adequacy.origin, Origin::Physician) { section.adequacy.value = Assessment::NotAssessed; } }
        if matches!(protocol.cervical_assessment.transformation_zone.origin, Origin::Physician) {
            protocol.cervical_assessment.transformation_zone.value = Assessment::NotAssessed;
        }
    }
}
