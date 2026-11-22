use m53_client::{defects::PRESETS, model::blank_protocol};
use m53_contracts::workflow::*;
use m53_domain::{protocol::*, validation::*};
use uuid::Uuid;
pub fn fixture() -> (StudyDetail, Dictionary, Uuid) {
    let id = Uuid::new_v4();
    let asset = Uuid::new_v4();
    let dictionary = Dictionary {
        version_id: id,
        clinically_approved: false,
        terms: PRESETS
            .iter()
            .filter_map(|p| {
                p.code.map(|code| Term {
                    code: code.into(),
                    label: p.name.into(),
                    region: Region::Cervix,
                    kind: TermKind::Finding,
                })
            })
            .collect(),
    };
    (
        StudyDetail {
            mis_task_bound: true,
            study: Study {
                id,
                patient_id: id,
                status: "draft".into(),
                row_version: 1,
                study_instance_uid: "2.25.1".into(),
            },
            patient: Patient { card_number: None,
                sex: None,
                id,
                external_id: None,
                family_name: "Тест".into(),
                given_name: "Тест".into(),
                patronymic: None,
                birth_date: None,
            },
            revision_id: Some(id),
            protocol: Some(blank_protocol(id)),
            annotations: vec![],
            assets: vec![Asset {
                id: asset,
                study_id: id,
                kind: "original".into(),
                sha256: "a".repeat(64),
                byte_size: 1,
                mime_type: "image/png".into(),
            }],
        },
        dictionary,
        asset,
    )
}
