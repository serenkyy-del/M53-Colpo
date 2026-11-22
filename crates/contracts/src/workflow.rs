use m53_domain::protocol::Protocol;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Login {
    pub login: String,
    pub password: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Session {
    pub user_id: Uuid,
    pub token: String,
    pub display_name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewPatient {
    pub family_name: String,
    pub given_name: String,
    pub patronymic: Option<String>,
    pub birth_date: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Patient {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_number: Option<String>,
    /// Identity supplied by the external patient system; never substitute the internal UUID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    pub family_name: String,
    pub given_name: String,
    pub patronymic: Option<String>,
    pub birth_date: Option<String>,
    /// Sex supplied by 1C, F/M/O; absent when unknown for a legacy/manual patient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sex: Option<crate::colposcopy_1c::Sex>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewStudy {
    #[serde(default)]
    pub order_id: Option<Uuid>,
    pub patient_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Study {
    pub id: Uuid,
    pub patient_id: Uuid,
    pub status: String,
    pub row_version: i64,
    pub study_instance_uid: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[allow(clippy::struct_field_names)]
pub struct Annotation {
    #[serde(default, skip_serializing_if = "AnnotationLayer::is_mark")]
    pub layer: AnnotationLayer,
    #[serde(default)]
    pub coordinate_system: AnnotationSpace,
    pub id: Uuid,
    pub asset_id: Uuid,
    pub lesion_id: Option<Uuid>,
    pub finding_id: Option<Uuid>,
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub label: String,
}
/// Non-destructive layers in protocol_annotations, never in the original asset bytes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AnnotationLayer {
    #[default]
    Mark,
    /// Arrow tail is x/y, tip is end_x/end_y in normalized original coordinates.
    Arrow { end_x: f32, end_y: f32 },
    Callout {
        end_x: f32,
        end_y: f32,
        full_name: String,
        term_code: Option<String>,
    },
    Target {
        rotation_degrees: f32,
        include_in_report: bool,
    },
    /// x/y are the top-left in EXIF-oriented original coordinates; radius is unused.
    OutputRegion { width: f32, height: f32 },
}
impl AnnotationLayer {
    pub fn is_mark(&self) -> bool {
        matches!(self, Self::Mark)
    }
}
impl Annotation {
    pub fn valid_layer(&self) -> bool {
        match self.layer {
            AnnotationLayer::Mark => true,
            AnnotationLayer::Arrow { end_x, end_y } => {
                end_x.is_finite()
                    && end_y.is_finite()
                    && (0.0..=1.0).contains(&end_x)
                    && (0.0..=1.0).contains(&end_y)
            }
            AnnotationLayer::Callout {
                end_x,
                end_y,
                ref full_name,
                ref term_code,
            } => {
                end_x.is_finite()
                    && end_y.is_finite()
                    && (0.0..=1.0).contains(&end_x)
                    && (0.0..=1.0).contains(&end_y)
                    && !full_name.trim().is_empty()
                    && full_name.len() <= 512
                    && term_code
                        .as_ref()
                        .is_none_or(|s| !s.is_empty() && s.len() <= 160)
            }
            AnnotationLayer::Target {
                rotation_degrees, ..
            } => rotation_degrees.is_finite() && (0.0..360.0).contains(&rotation_degrees),
            AnnotationLayer::OutputRegion { width, height } => {
                width.is_finite()
                    && height.is_finite()
                    && width > 0.0
                    && height > 0.0
                    && self.x + width <= 1.000001
                    && self.y + height <= 1.000001
            }
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveDraft {
    pub expected_version: i64,
    pub protocol: Protocol,
    pub annotations: Vec<Annotation>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StudyDetail {
    /// Authoritative task binding; absent on older servers/caches means unverified.
    #[serde(default)]
    pub mis_task_bound: bool,
    pub study: Study,
    pub patient: Patient,
    pub revision_id: Option<Uuid>,
    pub protocol: Option<Protocol>,
    pub annotations: Vec<Annotation>,
    pub assets: Vec<Asset>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Asset {
    pub id: Uuid,
    pub study_id: Uuid,
    pub kind: String,
    pub sha256: String,
    pub byte_size: i64,
    pub mime_type: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpectedVersion {
    pub expected_version: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportBlock {
    Patient,
    Indications,
    History,
    Assessment,
    Findings,
    Photos,
    Conclusion,
    Recommendations,
    Signature,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportLayout {
    pub title: String,
    pub blocks: Vec<ReportBlock>,
    pub photo_columns: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letterhead: Option<ReportLetterhead>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportLetterhead {
    pub logo_data_uri: String,
    pub patient_rows: Vec<ReportHeaderRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clinic: Option<ReportClinic>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportHeaderRow {
    pub label: String,
    pub field: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportClinic {
    pub guid: Uuid,
    pub name: String,
    pub license: String,
    pub phone: String,
    pub version: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Template {
    pub id: Uuid,
    pub version_id: Uuid,
    pub version: i32,
    pub layout: ReportLayout,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveReport {
    pub snapshot: StudyDetail,
    pub revision_id: Uuid,
    pub template_version_id: Uuid,
    pub html_asset_id: Uuid,
    pub pdf_asset_id: Uuid,
    pub client_version: String,
    pub renderer_version: String,
    pub manifest: Vec<Asset>,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Publication {
    #[serde(default)]
    pub notification_state: Option<String>,
    pub id: Uuid,
    pub study_id: Uuid,
    pub state: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalPatient {
    pub external_id: String,
    pub source_revision: i64,
    pub patient: NewPatient,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalOrder {
    pub external_order_id: String,
    pub external_patient_id: String,
    pub source_revision: i64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TerminologyVersion {
    pub id: Uuid,
    pub code: String,
    pub source: String,
    pub clinically_approved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Order {
    pub id: Uuid,
    pub patient_id: Uuid,
    pub external_order_id: String,
    pub status: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationSpace {
    #[default]
    OriginalOrientedNormalizedV1,
}

/// v2 symbols are derived from exact catalogued image annotations in both directions.
pub fn validate_colpo_links(
    protocol: &Protocol,
    annotations: &[Annotation],
) -> Result<(), &'static str> {
    let Some(colpo) = &protocol.colpo else {
        return Ok(());
    };
    let callouts: Vec<_> = annotations
        .iter()
        .filter(|a| matches!(a.layer, AnnotationLayer::Callout { .. }))
        .collect();
    if callouts.len() != colpo.marks.len() {
        return Err("colpo_annotation_mismatch");
    }
    for a in callouts {
        let mark = colpo.marks.get(&a.id).ok_or("colpo_mark_missing")?;
        let p = m53_domain::colpo::preset(&mark.catalog_id).ok_or("unknown_colpo_symbol")?;
        if let Some(fid) = a.finding_id {
            let owner = protocol
                .sections
                .iter()
                .flat_map(|s| &s.lesions)
                .find(|l| l.findings.iter().any(|f| f.id == fid))
                .map(|l| l.id);
            if owner != a.lesion_id {
                return Err("annotation_finding_owner_mismatch");
            }
        }
        if let Some(lesion_id) = a.lesion_id
            && !protocol
                .sections
                .iter()
                .flat_map(|s| &s.lesions)
                .any(|l| l.id == lesion_id && l.findings.iter().any(|f| Some(f.id) == a.finding_id))
        {
            return Err("invalid_annotation_lesion_finding");
        }
        if mark.asset_id != a.asset_id
            || p.short != a.label
            || !matches!(&a.layer,AnnotationLayer::Callout{full_name,..} if full_name==p.name)
        {
            return Err("colpo_annotation_mismatch");
        }
    }
    for section in &protocol.sections {
        for finding in section
            .findings
            .iter()
            .chain(section.lesions.iter().flat_map(|l| &l.findings))
        {
            if finding.observation.value != m53_domain::FeatureState::Present {
                return Err("annotation_requires_present_finding");
            }
            if !annotations.iter().any(|a| {
                a.finding_id == Some(finding.id)
                    && colpo.marks.get(&a.id).is_some_and(|m| {
                        m53_domain::colpo::preset(&m.catalog_id).is_some_and(|p| {
                            p.region == section.region && p.code == Some(finding.term_code.as_str())
                        })
                    })
            }) {
                return Err("symbol_requires_annotation");
            }
        }
    }
    Ok(())
}
