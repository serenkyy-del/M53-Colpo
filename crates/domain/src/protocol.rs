//! Versioned form model. Term definitions are supplied by a reviewed dictionary.
use crate::{ExaminationState, FeatureState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;
/// UTF-8 byte limit shared with the anamnesis phrase library.
pub const ANAMNESIS_MAX_BYTES: usize = 16_384;
pub const DIAGNOSES_MAX_ROWS: usize = 100;
pub const DIAGNOSIS_CODE_MAX_BYTES: usize = 32;
pub const DIAGNOSIS_NAME_MAX_BYTES: usize = 4096;
pub const DIAGNOSIS_ADDITIONAL_MAX_BYTES: usize = 16_384;
pub const DIAGNOSIS_NOTE_MAX_BYTES: usize = 16_384;

pub fn empty_history() -> Field<Assessment<String>> {
    Field { value: Assessment::NotAssessed, origin: Origin::Physician }
}

/// Missing is explicit; it never means a negative clinical observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(
    tag = "state",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Assessment<T> {
    Assessed(T),
    NotAssessed,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Origin {
    OneC { source: String, external_id: String },
    Physician,
    Annotation { annotation_id: Uuid },
    Calculation { method_version: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Field<T> {
    pub value: T,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    Cervix,
    Vagina,
    Vulva,
    Anus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Adequacy {
    Adequate,
    Inadequate { reason: String },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JunctionVisibility {
    FullyVisible,
    PartiallyVisible,
    NotVisible,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CervicalTransformationZone {
    Type1,
    Type2,
    Type3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ZoneRelation {
    Inside,
    Outside,
    Both,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VaginalThird {
    Upper,
    Middle,
    Lower,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VaginalWall {
    Anterior,
    Posterior,
    Left,
    Right,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tissue {
    HairBearingSkin,
    NonHairBearingSkin,
    Mucosa,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DentateLineRelation {
    Above,
    Below,
    Crossing,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "region", rename_all = "snake_case", deny_unknown_fields)]
pub enum Location {
    Cervix {
        zone_relation: Field<Assessment<ZoneRelation>>,
        clock_start: Field<Assessment<u8>>,
        clock_end: Field<Assessment<u8>>,
        quadrants: Field<Assessment<u8>>,
        affected_percent: Field<Assessment<f64>>,
    },
    Vagina {
        thirds: Field<Assessment<Vec<VaginalThird>>>,
        walls: Field<Assessment<Vec<VaginalWall>>>,
        zone_relation: Field<Assessment<ZoneRelation>>,
    },
    Vulva {
        anatomy_code: Field<Assessment<String>>,
        tissue: Field<Assessment<Tissue>>,
        length_cm: Field<Assessment<f64>>,
        width_cm: Field<Assessment<f64>>,
    },
    Anus {
        anatomy_code: Field<Assessment<String>>,
        dentate_line_relation: Field<Assessment<DentateLineRelation>>,
        length_cm: Field<Assessment<f64>>,
        width_cm: Field<Assessment<f64>>,
    },
}
impl Location {
    pub fn region(&self) -> Region {
        match self {
            Self::Cervix { .. } => Region::Cervix,
            Self::Vagina { .. } => Region::Vagina,
            Self::Vulva { .. } => Region::Vulva,
            Self::Anus { .. } => Region::Anus,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: Uuid,
    pub term_code: String,
    pub observation: Field<FeatureState>,
    /// Annotation proposals require an explicit physician confirmation for approval.
    pub physician_confirmed: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lesion {
    pub id: Uuid,
    pub location: Location,
    pub findings: Vec<Finding>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegionSection {
    pub region: Region,
    pub state: Field<ExaminationState>,
    pub adequacy: Field<Assessment<Adequacy>>,
    pub findings: Vec<Finding>,
    pub lesions: Vec<Lesion>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CervicalAssessment {
    pub junction_visibility: Field<Assessment<JunctionVisibility>>,
    pub transformation_zone: Field<Assessment<CervicalTransformationZone>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestApplication {
    Performed,
    NotPerformed,
    NotAssessed,
    NotApplicable,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProcedureTest {
    pub region: Region,
    pub code: String,
    pub application: Field<TestApplication>,
    pub result: Field<Assessment<String>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExcisionType {
    Type1,
    Type2,
    Type3,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExcisionSpecimen {
    pub id: Uuid,
    pub excision_type: Field<Assessment<ExcisionType>>,
    pub length_mm: Field<Assessment<f64>>,
    pub thickness_mm: Field<Assessment<f64>>,
    pub circumference_mm: Field<Assessment<f64>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExcisionSection {
    pub state: Field<ExaminationState>,
    pub specimens: Vec<ExcisionSpecimen>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Protocol {
    /// One optional physician note for the whole ordered diagnosis list.
    /// None is legacy omission; Some(NotAssessed) is an explicit clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnosis_note: Option<Field<Assessment<String>>>,
    /// None preserves legacy omission; Some([]) explicitly clears all rows.
    /// Selection snapshots and array order are immutable report content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnoses: Option<Vec<ProtocolDiagnosis>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colpo: Option<crate::colpo::Colpo>,
    #[serde(default)]
    pub clinical_context: Option<ClinicalContext>,
    #[serde(default)]
    pub biopsies: Vec<BiopsySpecimen>,
    pub schema_version: u32,
    pub terminology_version_id: Uuid,
    pub indications: Field<Assessment<String>>,
    /// Optional physician anamnesis. Wire name stays history for older clients.
    /// Empty assessed text is accepted for legacy roundtrips; new UI clears to NotAssessed.
    #[serde(default = "empty_history")]
    pub history: Field<Assessment<String>>,
    pub sections: Vec<RegionSection>,
    /// Kept separately so cervical zone types cannot be assigned to the vagina.
    pub cervical_assessment: CervicalAssessment,
    pub tests: Vec<ProcedureTest>,
    pub excision: ExcisionSection,
    pub conclusion: Field<Assessment<String>>,
    pub recommendations: Field<Assessment<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProtocolDiagnosis {
    pub id: Uuid,
    pub diagnosis_id: Uuid,
    pub code: String,
    pub name: String,
    pub additional_text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClinicalContext {
    pub cycle: Field<Assessment<String>>,
    pub pregnancy: Field<Assessment<String>>,
    pub menopause: Field<Assessment<String>>,
    pub method: Field<Assessment<String>>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BiopsySpecimen {
    pub id: Uuid,
    pub region: Region,
    pub lesion_id: Option<Uuid>,
    pub location: Field<Assessment<String>>,
    pub material: Field<Assessment<String>>,
    pub sent_for_histology: Field<crate::FeatureState>,
}
