use m53_domain::{ExaminationState, protocol::*};
use uuid::Uuid;
pub fn field<T>(value: T) -> Field<T> {
    Field {
        value,
        origin: Origin::Physician,
    }
}
fn missing<T>() -> Field<Assessment<T>> {
    field(Assessment::NotAssessed)
}
pub fn blank_protocol(version: Uuid) -> Protocol {
    Protocol {
        diagnosis_note: None,
        diagnoses: None,
        colpo: None,
        clinical_context: None,
        biopsies: vec![],
        schema_version: SCHEMA_VERSION,
        terminology_version_id: version,
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
        conclusion: missing(),
        recommendations: missing(),
    }
}
