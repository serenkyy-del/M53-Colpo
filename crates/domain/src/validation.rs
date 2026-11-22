//! Structural checks only: no diagnosis, treatment or inference from missing values.
use crate::protocol::*;
use crate::{ExaminationState, FeatureState};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Draft,
    Approval,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
    Finding,
    Anatomy,
    Test,
}
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Term {
    #[serde(default)]
    pub label: String,
    pub code: String,
    pub region: Region,
    pub kind: TermKind,
}
/// Loaded by the server, never trusted from a client request.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Dictionary {
    pub version_id: Uuid,
    pub clinically_approved: bool,
    pub terms: Vec<Term>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidationError {
    pub path: String,
    pub code: String,
}
struct Validator<'a> {
    errors: Vec<ValidationError>,
    dictionary: &'a Dictionary,
    mode: ValidationMode,
    ids: BTreeSet<Uuid>,
}
impl Validator<'_> {
    fn error(&mut self, path: &str, code: &str) {
        self.errors.push(ValidationError {
            path: path.into(),
            code: code.into(),
        });
    }
    fn id(&mut self, id: Uuid, path: &str) {
        if id.is_nil() {
            self.error(path, "nil_id");
        }
        if !self.ids.insert(id) {
            self.error(path, "duplicate_id");
        }
    }
    fn field<T>(&mut self, field: &Field<T>, path: &str) {
        match &field.origin {
            Origin::OneC {
                source,
                external_id,
            } if source.trim().is_empty() || external_id.trim().is_empty() => {
                self.error(path, "incomplete_origin")
            }
            Origin::Annotation { annotation_id } if annotation_id.is_nil() => {
                self.error(path, "nil_annotation_id")
            }
            Origin::Calculation { method_version } if method_version.trim().is_empty() => {
                self.error(path, "missing_method_version")
            }
            _ => {}
        }
    }
    fn term(&mut self, code: &str, region: Region, kind: TermKind, path: &str) {
        if !self
            .dictionary
            .terms
            .iter()
            .any(|t| t.code == code && t.region == region && t.kind == kind)
        {
            self.error(path, "unknown_or_inapplicable_term");
        }
    }
    fn number(&mut self, field: &Field<Assessment<f64>>, min: f64, max: f64, path: &str) {
        self.field(field, path);
        if let Assessment::Assessed(value) = field.value
            && (!value.is_finite() || !(min..=max).contains(&value))
        {
            self.error(path, "out_of_range");
        }
    }
    fn text(&mut self, field: &Field<Assessment<String>>, path: &str) {
        self.field(field, path);
        if let Assessment::Assessed(value) = &field.value
            && value.trim().is_empty()
        {
            self.error(path, "empty_assessed_value");
        }
    }
    fn findings(&mut self, findings: &[Finding], region: Region, prefix: &str) {
        let mut terms = BTreeSet::new();
        for (index, finding) in findings.iter().enumerate() {
            let path = format!("{prefix}[{index}]");
            self.id(finding.id, &format!("{path}.id"));
            self.term(
                &finding.term_code,
                region,
                TermKind::Finding,
                &format!("{path}.term_code"),
            );
            if !terms.insert(&finding.term_code) {
                self.error(&path, "duplicate_finding_term");
            }
            self.field(&finding.observation, &format!("{path}.observation"));
            if self.mode == ValidationMode::Approval
                && matches!(finding.observation.origin, Origin::Annotation { .. })
                && matches!(
                    finding.observation.value,
                    FeatureState::Present | FeatureState::Absent
                )
                && !finding.physician_confirmed
            {
                self.error(&path, "annotation_requires_physician_confirmation");
            }
        }
    }
    fn location(&mut self, location: &Location, path: &str) {
        match location {
            Location::Cervix {
                zone_relation,
                clock_start,
                clock_end,
                quadrants,
                affected_percent,
            } => {
                self.field(zone_relation, &format!("{path}.zone_relation"));
                for (name, field, min, max) in [
                    ("clock_start", clock_start, 1, 12),
                    ("clock_end", clock_end, 1, 12),
                    ("quadrants", quadrants, 0, 4),
                ] {
                    let p = format!("{path}.{name}");
                    self.field(field, &p);
                    if let Assessment::Assessed(value) = field.value
                        && !(min..=max).contains(&value)
                    {
                        self.error(&p, "out_of_range");
                    }
                }
                self.number(
                    affected_percent,
                    0.0,
                    100.0,
                    &format!("{path}.affected_percent"),
                );
            }
            Location::Vagina {
                thirds,
                walls,
                zone_relation,
            } => {
                self.field(thirds, &format!("{path}.thirds"));
                self.field(walls, &format!("{path}.walls"));
                self.field(zone_relation, &format!("{path}.zone_relation"));
                if let Assessment::Assessed(values) = &thirds.value
                    && (values.is_empty()
                        || values
                            .iter()
                            .enumerate()
                            .any(|(i, v)| values[..i].contains(v)))
                {
                    self.error(&format!("{path}.thirds"), "empty_or_duplicate_locations");
                }
                if let Assessment::Assessed(values) = &walls.value
                    && (values.is_empty()
                        || values
                            .iter()
                            .enumerate()
                            .any(|(i, v)| values[..i].contains(v)))
                {
                    self.error(&format!("{path}.walls"), "empty_or_duplicate_locations");
                }
            }
            Location::Vulva {
                anatomy_code,
                tissue,
                length_cm,
                width_cm,
            } => {
                self.anatomy(anatomy_code, Region::Vulva, &format!("{path}.anatomy_code"));
                self.field(tissue, &format!("{path}.tissue"));
                self.number(length_cm, 0.0, f64::MAX, &format!("{path}.length_cm"));
                self.number(width_cm, 0.0, f64::MAX, &format!("{path}.width_cm"));
            }
            Location::Anus {
                anatomy_code,
                dentate_line_relation,
                length_cm,
                width_cm,
            } => {
                self.anatomy(anatomy_code, Region::Anus, &format!("{path}.anatomy_code"));
                self.field(
                    dentate_line_relation,
                    &format!("{path}.dentate_line_relation"),
                );
                self.number(length_cm, 0.0, f64::MAX, &format!("{path}.length_cm"));
                self.number(width_cm, 0.0, f64::MAX, &format!("{path}.width_cm"));
            }
        }
    }
    fn anatomy(&mut self, field: &Field<Assessment<String>>, region: Region, path: &str) {
        self.text(field, path);
        if let Assessment::Assessed(code) = &field.value {
            self.term(code, region, TermKind::Anatomy, path);
        }
    }
}

pub fn validate(
    protocol: &Protocol,
    dictionary: &Dictionary,
    mode: ValidationMode,
) -> Vec<ValidationError> {
    let mut v = Validator {
        errors: vec![],
        dictionary,
        mode,
        ids: BTreeSet::new(),
    };
    if !matches!(protocol.schema_version, 1 | 2) {
        v.error("schema_version", "unsupported_schema_version");
    }
    if (protocol.schema_version == 2) != protocol.colpo.is_some() {
        v.error("colpo", "schema_extension_mismatch");
    }
    if let Some(colpo) = &protocol.colpo {
        for error in colpo.errors() {
            v.error("colpo", &error);
        }
    }
    if mode == ValidationMode::Approval {
        v.errors.extend(crate::approval_fields::validate(protocol));
    }
    if protocol.terminology_version_id.is_nil()
        || protocol.terminology_version_id != dictionary.version_id
    {
        v.error("terminology_version_id", "dictionary_version_mismatch");
    }
    if mode == ValidationMode::Approval && !dictionary.clinically_approved {
        v.error("terminology_version_id", "dictionary_not_approved");
    }
    if let Some(diagnoses) = &protocol.diagnoses {
        if diagnoses.len() > DIAGNOSES_MAX_ROWS {v.error("diagnoses", "too_many_diagnoses");}
        for (i,row) in diagnoses.iter().enumerate() {
            let path=format!("diagnoses[{i}]");
            v.id(row.id,&format!("{path}.id"));
            if row.diagnosis_id.is_nil() {v.error(&format!("{path}.diagnosis_id"),"nil_id");}
            for (key,value,max,required) in [
                ("code",row.code.as_str(),DIAGNOSIS_CODE_MAX_BYTES,true),
                ("name",row.name.as_str(),DIAGNOSIS_NAME_MAX_BYTES,true),
                ("additional_text",row.additional_text.as_str(),DIAGNOSIS_ADDITIONAL_MAX_BYTES,false),
            ] {
                if required && value.trim().is_empty() {v.error(&format!("{path}.{key}"),"empty_value");}
                if value.len()>max {v.error(&format!("{path}.{key}"),"text_too_long");}
            }
        }
    }
    if let Some(note) = &protocol.diagnosis_note {
        v.field(note, "diagnosis_note");
        if let Assessment::Assessed(text) = &note.value
            && text.len() > DIAGNOSIS_NOTE_MAX_BYTES
        {
            v.error("diagnosis_note", "text_too_long");
        }
    }
    v.text(&protocol.indications, "indications");
    // Optional anamnesis: preserve legacy empty/NotApplicable values without rewriting
    // immutable snapshots. Other text fields retain their usual empty-value checks.
    v.field(&protocol.history, "history");
    if let Assessment::Assessed(text) = &protocol.history.value
        && text.len() > ANAMNESIS_MAX_BYTES
    {
        v.error("history", "text_too_long");
    }
    v.text(&protocol.conclusion, "conclusion");
    v.text(&protocol.recommendations, "recommendations");
    if mode == ValidationMode::Approval
        && !matches!(&protocol.conclusion.value, Assessment::Assessed(s) if !s.trim().is_empty())
    {
        v.error("conclusion", "conclusion_required");
    }
    for region in [Region::Cervix, Region::Vagina, Region::Vulva, Region::Anus] {
        if protocol
            .sections
            .iter()
            .filter(|s| s.region == region)
            .count()
            != 1
        {
            v.error("sections", "each_region_required_once");
        }
    }
    for (i, section) in protocol.sections.iter().enumerate() {
        let path = format!("sections[{i}]");
        v.field(&section.state, &format!("{path}.state"));
        v.field(&section.adequacy, &format!("{path}.adequacy"));
        if section.state.value != ExaminationState::Examined {
            if !section.findings.is_empty()
                || !section.lesions.is_empty()
                || matches!(section.adequacy.value, Assessment::Assessed(_))
            {
                v.error(&path, "unexamined_section_has_observations");
            }
        } else if mode == ValidationMode::Approval
            && !(section.region == Region::Cervix
                && (protocol.schema_version == 2
                    || protocol.colpo.as_ref().is_some_and(|c| c.hidden())))
            && !matches!(section.adequacy.value, Assessment::Assessed(_))
        {
            v.error(&format!("{path}.adequacy"), "adequacy_required");
        }
        if let Assessment::Assessed(Adequacy::Inadequate { reason }) = &section.adequacy.value
            && reason.trim().is_empty()
        {
            v.error(&format!("{path}.adequacy"), "inadequacy_reason_required");
        }
        v.findings(
            &section.findings,
            section.region,
            &format!("{path}.findings"),
        );
        for (j, lesion) in section.lesions.iter().enumerate() {
            let p = format!("{path}.lesions[{j}]");
            v.id(lesion.id, &format!("{p}.id"));
            if lesion.location.region() != section.region {
                v.error(&format!("{p}.location"), "region_mismatch");
            }
            v.location(&lesion.location, &format!("{p}.location"));
            v.findings(&lesion.findings, section.region, &format!("{p}.findings"));
        }
    }
    let cervix_examined = protocol
        .sections
        .iter()
        .any(|s| s.region == Region::Cervix && s.state.value == ExaminationState::Examined);
    let cervical = &protocol.cervical_assessment;
    v.field(
        &cervical.junction_visibility,
        "cervical_assessment.junction_visibility",
    );
    v.field(
        &cervical.transformation_zone,
        "cervical_assessment.transformation_zone",
    );
    if !cervix_examined
        && (matches!(cervical.junction_visibility.value, Assessment::Assessed(_))
            || matches!(cervical.transformation_zone.value, Assessment::Assessed(_)))
    {
        v.error("cervical_assessment", "cervix_not_examined");
    }
    // Adequacy, junction visibility and zone are intentionally independent.
    // A valid document can contain each dictionary test/region pair only once.
    // Bound this subtree before inspecting untrusted rows, including unknown codes.
    let allowed_tests: BTreeSet<_> = dictionary.terms.iter()
        .filter(|term| term.kind == TermKind::Test)
        .map(|term| (term.region as u8, term.code.as_str()))
        .collect();
    let test_rows = if protocol.tests.len() > allowed_tests.len() {
        v.error("tests", "tests_too_many");
        &[][..]
    } else {
        protocol.tests.as_slice()
    };
    let mut tests = BTreeSet::new();
    for (i, test) in test_rows.iter().enumerate() {
        let path = format!("tests[{i}]");
        v.term(
            &test.code,
            test.region,
            TermKind::Test,
            &format!("{path}.code"),
        );
        if !tests.insert((test.region as u8, test.code.as_str())) {
            v.error(&path, "duplicate_test");
        }
        v.field(&test.application, &format!("{path}.application"));
        v.text(&test.result, &format!("{path}.result"));
        if matches!(test.result.value, Assessment::Assessed(_))
            && test.application.value != TestApplication::Performed
        {
            v.error(&format!("{path}.result"), "result_without_performed_test");
        }
        if test.application.value == TestApplication::Performed
            && !protocol
                .sections
                .iter()
                .any(|s| s.region == test.region && s.state.value == ExaminationState::Examined)
        {
            v.error(&path, "test_in_unexamined_region");
        }
    }
    v.field(&protocol.excision.state, "excision.state");
    if protocol.excision.state.value != ExaminationState::Examined
        && !protocol.excision.specimens.is_empty()
    {
        v.error("excision.specimens", "unexamined_section_has_observations");
    }
    if mode == ValidationMode::Approval
        && protocol.excision.state.value == ExaminationState::Examined
        && protocol.excision.specimens.is_empty()
    {
        v.error("excision.specimens", "specimen_required");
    }
    for (i, specimen) in protocol.excision.specimens.iter().enumerate() {
        let path = format!("excision.specimens[{i}]");
        v.id(specimen.id, &format!("{path}.id"));
        v.field(&specimen.excision_type, &format!("{path}.excision_type"));
        v.number(
            &specimen.length_mm,
            0.0,
            f64::MAX,
            &format!("{path}.length_mm"),
        );
        v.number(
            &specimen.thickness_mm,
            0.0,
            f64::MAX,
            &format!("{path}.thickness_mm"),
        );
        v.number(
            &specimen.circumference_mm,
            0.0,
            f64::MAX,
            &format!("{path}.circumference_mm"),
        );
        if mode == ValidationMode::Approval
            && (!matches!(specimen.excision_type.value, Assessment::Assessed(_))
                || !matches!(specimen.length_mm.value, Assessment::Assessed(_))
                || !matches!(specimen.thickness_mm.value, Assessment::Assessed(_)))
        {
            v.error(&path, "specimen_dimensions_and_type_required");
        }
    }
    if let Some(context) = &protocol.clinical_context {
        v.text(&context.cycle, "clinical_context.cycle");
        v.text(&context.pregnancy, "clinical_context.pregnancy");
        v.text(&context.menopause, "clinical_context.menopause");
        v.text(&context.method, "clinical_context.method");
    }
    for (i, specimen) in protocol.biopsies.iter().enumerate() {
        let path = format!("biopsies[{i}]");
        v.id(specimen.id, &format!("{path}.id"));
        v.text(&specimen.location, &format!("{path}.location"));
        v.text(&specimen.material, &format!("{path}.material"));
        v.field(
            &specimen.sent_for_histology,
            &format!("{path}.sent_for_histology"),
        );
        let section = protocol
            .sections
            .iter()
            .find(|s| s.region == specimen.region);
        if !section.is_some_and(|s| s.state.value == ExaminationState::Examined) {
            v.error(&path, "biopsy_in_unexamined_region");
        }
        if specimen
            .lesion_id
            .is_some_and(|id| !section.is_some_and(|s| s.lesions.iter().any(|l| l.id == id)))
        {
            v.error(&path, "biopsy_lesion_region_mismatch");
        }
    }
    v.errors
}
