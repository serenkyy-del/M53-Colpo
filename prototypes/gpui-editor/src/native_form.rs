//! Full schema-driven native protocol controls. No browser, HTML, or executable templates.
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Selectable, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::{prelude::*, *};
use m53_domain::{protocol::Region, validation::Dictionary};
use m53_gpui_editor::schema;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

#[path = "form_ergonomics.rs"]
pub(crate) mod ergonomics;
#[path = "diagnosis_form.rs"]
mod diagnosis_form;

/// A semantic library key and the exact destination in this instance of the form.
/// A template click keeps this snapshot when focus moves into the side panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhraseTarget {
    pub field_code: String,
    pub path: String,
    pub label: String,
    pub mark_id: Option<uuid::Uuid>,
    pub max_bytes: usize,
    form_id: uuid::Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhraseApplyResult {
    pub changed: bool,
    pub field_code: String,
    pub mark_id: Option<uuid::Uuid>,
}

const PHRASE_TEXT_MAX_BYTES: usize = 16_384;

fn adequacy_path(value: &Value, region: Region) -> Option<String> {
    let region = match region {
        Region::Cervix => "cervix",
        Region::Vagina => "vagina",
        Region::Vulva => "vulva",
        Region::Anus => "anus",
    };
    let index = value["sections"].as_array()?
        .iter().position(|section| section["region"] == region)?;
    Some(format!("/sections/{index}/adequacy/value"))
}

#[cfg(test)]
mod adequacy_tests {
    use super::adequacy_path;
    use m53_domain::protocol::Region;
    use serde_json::{Value, json};

    #[test]
    fn adequacy_destination_follows_region_for_every_section_order() {
        let names = ["cervix", "vagina", "vulva", "anus"];
        let mut orders = 0;
        for a in 0..4 { for b in 0..4 { for c in 0..4 { for d in 0..4 {
            let order = [a, b, c, d];
            if order.iter().collect::<std::collections::BTreeSet<_>>().len() != 4 { continue; }
            orders += 1;
            let value = json!({"sections":order.map(|index| json!({
                "region":names[index],
                "adequacy":{"value":{"state":"assessed","value":{"state":"inadequate","reason":names[index]}}}
            }))});
            for (region, name) in [(Region::Cervix,"cervix"), (Region::Vulva,"vulva"), (Region::Anus,"anus")] {
                let path = adequacy_path(&value, region).expect("the requested region exists");
                assert_eq!(value.pointer(&path).unwrap()["value"]["reason"], name,
                    "wrong region for {name} with section order {order:?}");
                let mut edited = value.clone();
                *edited.pointer_mut(&path).unwrap() = json!({"state":"assessed","value":{"state":"adequate"}});
                for (before, after) in value["sections"].as_array().unwrap().iter()
                    .zip(edited["sections"].as_array().unwrap())
                {
                    if before["region"] == name {
                        assert_eq!(after["adequacy"]["value"]["value"]["state"],"adequate");
                    } else {
                        assert_eq!(after,before,"editing {name} changed an unrelated section");
                    }
                }
            }
        }}}}
        assert_eq!(orders,24);
    }

    #[test]
    fn absent_adequacy_region_has_no_destination() {
        let value: Value = json!({"sections":[{"region":"vulva","adequacy":{"value":{"state":"not_assessed"}}}]});
        assert!(adequacy_path(&value,Region::Cervix).is_none());
        assert!(adequacy_path(&json!({}),Region::Anus).is_none());
    }
}

fn prepare_text_edit(value: &mut Value, path: &str, text: &str) -> bool {
    if path == "/diagnosis_note/value" && value["diagnosis_note"].is_null() {
        if text.trim().is_empty() { return false; }
        value["diagnosis_note"] = serde_json::to_value(m53_domain::protocol::empty_history()).unwrap();
    }
    true
}

fn phrase_target(value: &Value, path: &str, form_id: uuid::Uuid) -> Option<PhraseTarget> {
    use m53_domain::colpo::{Control, FIELDS};
    let parts: Vec<_> = path.strip_prefix('/')?.split('/').collect();
    let cervical_hidden = ["context.no_cervix", "context.skip"]
        .iter()
        .any(|id| value["colpo"]["fields"][id]["value"] == true);
    let (field_code, label, mark_id) = match parts.as_slice() {
        ["history", "value"] if value["history"].is_object() => {
            ("anamnesis".to_string(), "Анамнез".to_string(), None)
        }
        ["diagnosis_note", "value"]
            if value.get("diagnosis_note").is_none_or(|v| v.is_null() || v.is_object()) =>
        {
            ("diagnosis_note".to_string(), "Дополнение к диагнозам".to_string(), None)
        }
        ["conclusion", "value"] if value["conclusion"].is_object() => {
            ("conclusion".to_string(), "Заключение врача".to_string(), None)
        }
        ["colpo", "fields", code, "value"]
            if value["colpo"]["fields"].is_object() =>
        {
            let field = FIELDS.iter().find(|f| {
                f.id == *code && f.parent.is_empty() && matches!(f.control, Control::Text)
            })?;
            if cervical_hidden && !field.id.starts_with("vulva.") {
                return None;
            }
            (field.id.to_string(), field.label.to_string(), None)
        }
        ["colpo", "marks", id, "properties", code, "value"] => {
            let id = id.parse::<uuid::Uuid>().ok()?;
            let mark = value["colpo"]["marks"].get(id.to_string())?;
            let catalog_id = mark["catalog_id"].as_str()?;
            let preset = m53_domain::colpo::preset(catalog_id)?;
            let field = FIELDS.iter().find(|f| {
                f.id == *code && f.parent == catalog_id && matches!(f.control, Control::Text)
            })?;
            if !mark["properties"].is_object() {
                return None;
            }
            (
                format!("{catalog_id}.{}", field.id),
                format!("{} · {}", preset.name, field.label),
                Some(id),
            )
        }
        ["sections", index, "adequacy", "value", "value", "reason"] => {
            let index = index.parse::<usize>().ok()?;
            let section = value["sections"].as_array()?.get(index)?;
            let adequacy = &section["adequacy"]["value"];
            if adequacy["state"] != "assessed" || adequacy["value"]["state"] != "inadequate" {
                return None;
            }
            let (region, caption) = match section["region"].as_str()? {
                "cervix" if !cervical_hidden => ("cervix", "шейки матки"),
                "vulva" => ("vulva", "вульвы"),
                "anus"
                    if value["colpo"]["marks"].as_object().is_some_and(|marks| {
                        marks.values().any(|m| m["catalog_id"] == "vulva.magnification.analjunction")
                    }) => ("anus", "анальной области"),
                _ => return None,
            };
            (
                format!("{region}.adequacy_reason"),
                format!("Причина неадекватности оценки {caption}"),
                None,
            )
        }
        _ => return None,
    };
    Some(PhraseTarget {
        field_code,
        path: path.to_string(),
        label,
        mark_id,
        max_bytes: PHRASE_TEXT_MAX_BYTES,
        form_id,
    })
}

fn phrase_text(current: &str, text: &str, replace: bool, max_bytes: usize) -> Result<String, String> {
    if text.trim().is_empty() || text.contains('\0') {
        return Err("Шаблон должен содержать текст без недопустимых символов".into());
    }
    let separator = !replace && !current.is_empty() && !current.ends_with('\n') && !text.starts_with('\n');
    let length = if replace { 0 } else { current.len() }
        .checked_add(usize::from(separator))
        .and_then(|n| n.checked_add(text.len()))
        .filter(|n| *n <= max_bytes)
        .ok_or_else(|| format!("Текст поля не должен превышать {max_bytes} байт UTF-8"))?;
    let mut result = String::with_capacity(length);
    if !replace {
        result.push_str(current);
        if separator { result.push('\n'); }
    }
    result.push_str(text);
    Ok(result)
}

#[cfg(test)]
mod phrase_tests {
    use super::{PHRASE_TEXT_MAX_BYTES, phrase_target, phrase_text};
    use serde_json::{Value, json};
    use uuid::Uuid;

    fn form() -> Value {
        json!({
            "history":{"value":{"state":"not_assessed"}},
            "conclusion":{"value":{"state":"not_assessed"}},
            "colpo":{"fields":{},"marks":{}},
            "sections":[
                {"region":"cervix","adequacy":{"value":{"state":"assessed","value":{"state":"inadequate","reason":""}}}},
                {"region":"vulva","adequacy":{"value":{"state":"assessed","value":{"state":"inadequate","reason":""}}}},
                {"region":"anus","adequacy":{"value":{"state":"assessed","value":{"state":"inadequate","reason":""}}}}
            ]
        })
    }

    #[test]
    fn all_nineteen_field_families_have_distinct_semantic_keys() {
        let mut value = form();
        let form_id = Uuid::new_v4();
        let anal_id = Uuid::new_v4();
        value["colpo"]["marks"][anal_id.to_string()] = json!({"catalog_id":"vulva.magnification.analjunction","properties":{}});
        let mut keys = std::collections::BTreeSet::new();
        for code in ["extended.clock", "extended.other", "rectovaginal.description", "vulva.other_comment", "vulva.cancer_comment"] {
            let target = phrase_target(&value, &format!("/colpo/fields/{code}/value"), form_id).unwrap();
            assert_eq!(target.field_code, code);
            assert!(target.mark_id.is_none());
            keys.insert(target.field_code);
        }
        keys.insert(phrase_target(&value, "/conclusion/value", form_id).unwrap().field_code);
        keys.insert(phrase_target(&value, "/history/value", form_id).unwrap().field_code);
        for (index, region) in ["cervix", "vulva", "anus"].iter().enumerate() {
            let target = phrase_target(&value, &format!("/sections/{index}/adequacy/value/value/reason"), form_id).unwrap();
            assert_eq!(target.field_code, format!("{region}.adequacy_reason"));
            keys.insert(target.field_code);
        }
        for catalog_id in [
            "vulva.other.trauma", "vulva.other.malformation", "vulva.other.hematoma", "vulva.other.hemangioma",
            "vulva.cancer.neoplasia", "vulva.cancer.ulceration", "vulva.cancer.necrosis", "vulva.cancer.bleeding",
            "vulva.cancer.hyperkeratosis", "vulva.cancer.exophytic",
        ] {
            let id = Uuid::new_v4();
            value["colpo"]["marks"][id.to_string()] = json!({"catalog_id":catalog_id,"properties":{}});
            let target = phrase_target(&value, &format!("/colpo/marks/{id}/properties/comment/value"), form_id).unwrap();
            assert_eq!(target.field_code, format!("{catalog_id}.comment"));
            assert_eq!(target.mark_id, Some(id));
            assert!(!target.field_code.contains(&id.to_string()));
            keys.insert(target.field_code);
        }
        assert_eq!(keys.len(), 20);
    }

    #[test]
    fn target_snapshot_rejects_deleted_retagged_and_reordered_destinations() {
        let mut value = form();
        let form_id = Uuid::new_v4();
        let id = Uuid::new_v4();
        value["colpo"]["marks"][id.to_string()] = json!({"catalog_id":"vulva.other.trauma","properties":{}});
        let path = format!("/colpo/marks/{id}/properties/comment/value");
        let target = phrase_target(&value, &path, form_id).unwrap();
        assert_ne!(phrase_target(&value, &path, Uuid::new_v4()).as_ref(), Some(&target));
        value["colpo"]["marks"][id.to_string()]["catalog_id"] = json!("vulva.other.hematoma");
        assert_ne!(phrase_target(&value, &path, form_id).as_ref(), Some(&target));
        value["colpo"]["marks"].as_object_mut().unwrap().remove(&id.to_string());
        assert!(phrase_target(&value, &path, form_id).is_none());
        let path = "/sections/0/adequacy/value/value/reason";
        let target = phrase_target(&value, path, form_id).unwrap();
        value["sections"].as_array_mut().unwrap().swap(0, 1);
        assert_ne!(phrase_target(&value, path, form_id).as_ref(), Some(&target));
        assert_eq!(target.field_code, "cervix.adequacy_reason");
    }

    #[test]
    fn common_diagnosis_note_has_an_independent_phrase_destination_without_selected_rows() {
        let mut value = form();
        value["diagnoses"] = json!([]);
        value["diagnosis_note"] = json!({"value":{"state":"not_assessed"}});
        let form_id = Uuid::new_v4();
        let target = phrase_target(&value, "/diagnosis_note/value", form_id)
            .expect("the optional common note must support templates even with no diagnosis rows");
        assert_eq!(target.field_code, "diagnosis_note");
        assert_eq!(target.label, "Дополнение к диагнозам");
        assert_eq!(target.max_bytes, PHRASE_TEXT_MAX_BYTES);
        assert!(target.mark_id.is_none());
        value["diagnoses"] = json!([{"id":Uuid::new_v4()}]);
        assert_eq!(phrase_target(&value, &target.path, form_id), Some(target.clone()));
        value.as_object_mut().unwrap().remove("diagnosis_note");
        assert_eq!(phrase_target(&value, &target.path, form_id), Some(target.clone()));
        value["diagnosis_note"] = Value::Null;
        assert_eq!(phrase_target(&value, &target.path, form_id), Some(target));
        assert!(phrase_target(&value, "/diagnoses/0/additional_text", form_id).is_none());
    }

    #[test]
    fn common_diagnosis_note_first_empty_input_preserves_absent_or_null_but_present_clear_is_an_edit() {
        for original in [json!({}), json!({"diagnosis_note":null})] {
            for text in ["", " \n\t "] {
                let mut value = original.clone();
                assert!(!super::prepare_text_edit(&mut value, "/diagnosis_note/value", text));
                assert_eq!(value, original, "empty initial input must preserve the frozen optional representation");
            }
            let mut value = original;
            assert!(super::prepare_text_edit(&mut value, "/diagnosis_note/value", "Ввод врача"));
            assert!(value["diagnosis_note"].is_object());
        }
        let mut present = json!({"diagnosis_note":{"value":{"state":"assessed","value":"Ввод врача"},"origin":{"kind":"physician"}}});
        let original = present.clone();
        assert!(super::prepare_text_edit(&mut present, "/diagnosis_note/value", ""),
            "clearing an existing note must continue through the normal NotAssessed save");
        assert_eq!(present, original, "preparing a clear must not erase the value before generation and audit are recorded");
    }

    #[test]
    fn common_diagnosis_note_cleanup_saves_editable_legacy_rows_without_rewriting_the_base() {
        let mut value = form();
        value["diagnoses"] = json!([{
            "id":Uuid::new_v4(),"diagnosis_id":Uuid::new_v4(),
            "code":"N87","name":"Синтетический диагноз","additional_text":"Старое тестовое дополнение"
        }]);
        let original = value.clone();
        let dictionary = m53_domain::validation::Dictionary {
            version_id: Uuid::new_v4(), clinically_approved: false, terms: vec![]
        };
        let editable = super::NativeForm::new(value.clone(), dictionary.clone(), Default::default(), false);
        assert_eq!(editable.value["diagnoses"][0]["additional_text"], "");
        assert_eq!(editable.base, original, "the incoming protocol remains the immutable comparison base");
        assert_eq!(editable.generation, 1, "the approved test-data cleanup goes through ordinary durable autosave");
        assert!(editable.audit_fields.contains("/diagnoses"));
        let readonly = super::NativeForm::new(value, dictionary, Default::default(), true);
        assert_eq!(readonly.value, original);
        assert_eq!(readonly.generation, 0);
    }

    #[test]
    fn common_diagnosis_note_does_not_dirty_an_untouched_legacy_protocol_and_discards_only_old_unfinished_additions() {
        let value = form();
        let dictionary = m53_domain::validation::Dictionary {
            version_id: Uuid::new_v4(), clinically_approved: false, terms: vec![]
        };
        let untouched = super::NativeForm::new(value.clone(), dictionary.clone(), Default::default(), false);
        assert_eq!(untouched.value, value);
        assert_eq!(untouched.generation, 0);
        let old_path = format!("/diagnoses/{}/additional_text", Uuid::new_v4());
        let unfinished: std::collections::BTreeMap<String, String> = [(old_path.clone(), "Старое дополнение".into()),
            ("/conclusion/value".into(), "Ввод заключения".into())].into_iter().collect();
        let draft = super::NativeForm::new(value.clone(), dictionary.clone(), unfinished.clone(), false);
        assert_eq!(draft.generation, 1);
        assert!(!draft.unfinished.contains_key(&old_path));
        assert_eq!(draft.unfinished["/conclusion/value"], "Ввод заключения");
        let readonly = super::NativeForm::new(value, dictionary, unfinished.clone(), true);
        assert_eq!(readonly.unfinished, unfinished);
        assert_eq!(readonly.generation, 0);
    }

    #[test]
    fn only_visible_eligible_text_definitions_can_be_destinations() {
        let mut value = form();
        let id = Uuid::new_v4();
        for path in [ "/recommendations/value", "/colpo/fields/context.lmp/value", "/colpo/fields/simple.relief/value", "/conclusion", "/sections/99/adequacy/value/value/reason"] {
            assert!(phrase_target(&value, path, id).is_none(), "{path}");
        }
        assert!(phrase_target(&value, "/sections/2/adequacy/value/value/reason", id).is_none());
        value["sections"][0]["adequacy"]["value"]["value"]["state"] = json!("adequate");
        assert!(phrase_target(&value, "/sections/0/adequacy/value/value/reason", id).is_none());
        value["colpo"]["fields"]["context.skip"] = json!({"kind":"checked","value":true});
        assert!(phrase_target(&value, "/colpo/fields/extended.other/value", id).is_none());
        assert!(phrase_target(&value, "/colpo/fields/vulva.other_comment/value", id).is_some());
        assert!(phrase_target(&value, "/conclusion/value", id).is_some());
    }

    #[test]
    fn append_and_replace_preserve_existing_text_and_use_one_line_separator() {
        assert_eq!(phrase_text("Ввод врача", "Готовая фраза", false, 16384).unwrap(), "Ввод врача\nГотовая фраза");
        assert_eq!(phrase_text("Ввод врача\n", "Готовая фраза", false, 16384).unwrap(), "Ввод врача\nГотовая фраза");
        assert_eq!(phrase_text("Ввод врача", "\nГотовая фраза", false, 16384).unwrap(), "Ввод врача\nГотовая фраза");
        assert_eq!(phrase_text("", "Готовая фраза", false, 16384).unwrap(), "Готовая фраза");
        assert_eq!(phrase_text("Ввод врача", "Готовая фраза", true, 16384).unwrap(), "Готовая фраза");
        assert!(phrase_text("Ввод врача", " \n", false, 16384).is_err());
        assert!(phrase_text("Ввод врача", "фраза\0", true, 16384).is_err());
    }

    #[test]
    fn utf8_byte_limit_is_checked_on_final_field_value_without_truncation() {
        let exact = "я".repeat(PHRASE_TEXT_MAX_BYTES / 2);
        assert_eq!(phrase_text("", &exact, false, PHRASE_TEXT_MAX_BYTES).unwrap(), exact);
        assert!(phrase_text("a", &exact, false, PHRASE_TEXT_MAX_BYTES).is_err());
        assert!(phrase_text(&exact, "я", false, PHRASE_TEXT_MAX_BYTES).is_err());
        assert_eq!(phrase_text(&exact, "я", true, PHRASE_TEXT_MAX_BYTES).unwrap(), "я");
        assert!(phrase_text("", &format!("{exact}я"), true, PHRASE_TEXT_MAX_BYTES).is_err());
    }
}

fn readonly_value(text: impl Into<SharedString>, cx: &App) -> Div {
    div().w_full().min_w_0().min_h(px(26.)).px_2().py_1()
        .rounded_md().border_1().border_color(cx.theme().border)
        .text_sm().text_color(cx.theme().foreground).child(text.into())
}

fn protocol_heading(title: &str, cx: &App) -> Div {
    div()
        .w_full()
        .px_2()
        .py_1()
        .font_semibold()
        .bg(cx.theme().secondary)
        .text_color(cx.theme().foreground)
        .border_1()
        .border_color(cx.theme().border)
        .child(title.to_owned())
}

struct FieldNavigation {
    path:String,
    anchor:ScrollAnchor,
    focus:FocusHandle,
    pending:bool,
}
pub struct NativeForm {
    pub value: Value,
    pub base: Value,
    pub focus_mark: Option<uuid::Uuid>,
    pub show_mark: Option<uuid::Uuid>,
    pub photo_order: Vec<uuid::Uuid>,
    dates: BTreeMap<String, Entity<gpui_kit::component::date_picker::DatePickerState>>,
    colpo_expanded: [Option<usize>; 2],
    anamnesis_expanded: bool,
    expanded_mark: Option<uuid::Uuid>,
    pub unfinished: BTreeMap<String, String>,
    pub generation: u64,
    pub changed: Instant,
    pub audit_fields: std::collections::BTreeSet<String>,
    pub audit_sections: Vec<String>,
    phrase_form_id: uuid::Uuid,
    active_phrase: Option<PhraseTarget>,
    phrase_scroll: ScrollHandle,
    navigation:Option<FieldNavigation>,
    inputs: BTreeMap<String, Entity<InputState>>,
    areas: BTreeMap<String, Entity<TextareaState>>,
    subscriptions: Vec<Subscription>,
    pub readonly: bool,
    pub compact_marker_card: bool,
    dictionary: Dictionary,
    validation_errors: Vec<(String, String)>,
    validation_revision: Option<(u64,bool)>,
    compact_context: bool,
    diagnoses: diagnosis_form::Ui,
}
impl NativeForm {
    /// Replace portable physician fields without replacing the current patient's dates or images.
    /// A partially entered number must be resolved before this operation.
    pub fn replace_visit_fields(&mut self, template: Option<&m53_domain::visit_template::VisitTemplate>, cx: &mut Context<Self>) -> Result<(), String> {
        if self.readonly { return Err("Утверждённый протокол нельзя изменить".into()); }
        if !self.unfinished.is_empty() { return Err("Завершите ввод в незаполненных полях перед применением или очисткой шаблона".into()); }
        let mut protocol: m53_domain::protocol::Protocol = serde_json::from_value(self.value.clone()).map_err(|_| "Не удалось прочитать протокол".to_string())?;
        match template { Some(template) => template.apply(&mut protocol)?, None => m53_domain::visit_template::VisitTemplate::clear(&mut protocol) }
        let value = serde_json::to_value(protocol).map_err(|_| "Не удалось подготовить протокол".to_string())?;
        if value == self.value { return Ok(()); }
        let mut next = NativeForm::new(value, self.dictionary.clone(), self.unfinished.clone(), false);
        next.base = self.value.clone();
        next.generation = self.generation.checked_add(1).ok_or("Не удалось обновить протокол")?;
        next.photo_order = self.photo_order.clone();
        next.focus_mark = self.focus_mark;
        next.colpo_expanded = self.colpo_expanded;
        next.anamnesis_expanded = self.anamnesis_expanded;
        next.compact_context = self.compact_context;
        for name in ["history", "conclusion", "diagnosis_note", "diagnoses", "sections", "cervical_assessment"] {
            if self.value[name] != next.value[name] { next.audit_fields.insert(format!("/{name}")); }
        }
        let before=self.value.pointer("/colpo/fields").and_then(Value::as_object);
        let after=next.value.pointer("/colpo/fields").and_then(Value::as_object);
        for key in before.into_iter().flat_map(|fields|fields.keys()).chain(after.into_iter().flat_map(|fields|fields.keys())) {
            if before.and_then(|fields|fields.get(key)) != after.and_then(|fields|fields.get(key)) {
                next.audit_fields.insert(format!("/colpo/fields/{key}"));
            }
        }
        *self = next;
        cx.notify();
        Ok(())
    }
    #[cfg(not(windows))]
    pub(crate) fn preview_date_state(&self, field: &str) -> Entity<gpui_kit::component::date_picker::DatePickerState> {
        self.dates[&format!("/colpo/fields/{field}")].clone()
    }

    /// Width of the form area, excluding the persistent phrase panel.
    /// Layout changes never move focus or scroll an already open field.
    pub fn set_available_width(&mut self, width: f32, cx: &mut Context<Self>) {
        let compact = ergonomics::compact_context(width);
        if self.compact_context != compact {
            self.compact_context = compact;
            cx.notify();
        }
    }

    /// Populate the revision cache on first use, including before the child renders.
    pub fn approval_navigation(&mut self) -> Vec<(String, String)> {
        self.ensure_validation();
        if self.readonly { return Vec::new(); }
        self.validation_errors.iter().filter_map(|(path, message)| {
            let pointer = m53_client::workspace::validation_pointer(path);
            self.problem_target(&pointer).map(|_| (pointer, message.clone()))
        }).collect()
    }

    fn problem_target(&self, path: &str) -> Option<String> {
        if path == "/conclusion" || path.starts_with("/conclusion/") { return Some("conclusion".into()); }
        if path == "/history" || path.starts_with("/history/") { return Some("anamnesis".into()); }
        if path == "/diagnoses" || path.starts_with("/diagnoses/") || path == "/diagnosis_note" || path.starts_with("/diagnosis_note/") { return Some("diagnoses".into()); }
        if let Some((column, group)) = self.problem_group(path) {
            let hidden = column == 0 && ["context.no_cervix", "context.skip"].iter()
                .any(|id| self.value["colpo"]["fields"][*id]["value"] == true);
            return (!hidden).then(|| format!("group:{column}:{group}"));
        }
        if let Some(index) = path.strip_prefix("/sections/").and_then(|p| p.split('/').next())
            .and_then(|i| i.parse::<usize>().ok()) {
            if matches!(self.value["sections"][index]["region"].as_str(), Some("vulva" | "anus")) {
                return Some("vulva-heading".into());
            }
        }
        None
    }

    /// Explicit navigation changes only view state, including in a read-only form.
    /// Both the summary and this command use the same supported heading targets.
    pub fn navigate_problem(&mut self, path: &str, cx: &mut Context<Self>) -> bool {
        let pointer = m53_client::workspace::validation_pointer(path);
        let Some(target) = self.problem_target(&pointer) else { return false; };
        if target == "anamnesis" {
            self.anamnesis_expanded = true;
        } else if let Some((column, group)) = self.problem_group(&pointer) {
            self.colpo_expanded = [None, None];
            self.colpo_expanded[column] = Some(group);
            self.expanded_mark = pointer.strip_prefix("/colpo/marks/")
                .and_then(|p| p.split('/').next()).and_then(|id| id.parse().ok());
            self.active_phrase = None;
        }
        self.navigate_section(target, cx);
        cx.notify();
        true
    }

    fn navigate_group(&mut self,column:usize,group:usize,cx:&mut Context<Self>) {
        self.navigate_section(format!("group:{column}:{group}"),cx);
    }
    fn navigate_section(&mut self,path:String,cx:&mut Context<Self>) {
        // A section opens at its heading, independently of validation errors.
        self.navigation=Some(FieldNavigation {path,
            anchor:ScrollAnchor::for_handle(self.phrase_scroll.clone()),focus:cx.focus_handle(),pending:true});
    }
    pub fn invalidate_validation(&mut self) { self.validation_revision = None; }
    fn ensure_validation(&mut self) {
        let revision = (self.generation, self.readonly);
        if self.validation_revision == Some(revision) { return; }
        self.validation_revision = Some(revision);
        self.validation_errors =
            serde_json::from_value::<m53_domain::protocol::Protocol>(self.value.clone())
                .ok()
                .filter(|_| !self.readonly)
                .map(|protocol| {
                    m53_domain::validation::validate(
                        &protocol,
                        &self.dictionary,
                        m53_domain::validation::ValidationMode::Approval,
                    )
                    .iter()
                    .map(|e| (
                        m53_client::workspace::validation_pointer(&e.path),
                        m53_client::workspace::approval_error_message(&protocol, e),
                    ))
                    .collect()
                })
                .unwrap_or_default();
    }
    pub fn new(
        mut value: Value,
        dictionary: Dictionary,
        mut unfinished: BTreeMap<String, String>,
        readonly: bool,
    ) -> Self {
        let base = value.clone();
        let normalized = m53_client::diagnosis_rows::prepare_common_note(&mut value, readonly);
        let unfinished_count = unfinished.len();
        if !readonly {
            // The user explicitly discarded the old per-row test additions.
            // Keep unrelated unfinished input; a hidden old row field must not
            // prevent ordinary autosave of the new common note.
            unfinished.retain(|path, _| !(path.starts_with("/diagnoses/") && path.ends_with("/additional_text")));
        }
        let changed = normalized || unfinished.len() != unfinished_count;
        Self {
            focus_mark: None,
            show_mark: None,
            photo_order: vec![],
            dates: BTreeMap::new(),
            colpo_expanded: [None, None],
            anamnesis_expanded: false,
            expanded_mark: None,
            base,
            value,
            unfinished,
            generation: u64::from(changed),
            changed: Instant::now(),
            audit_fields: if changed { ["/diagnoses".into()].into_iter().collect() } else { Default::default() },
            audit_sections: vec![],
            phrase_form_id: uuid::Uuid::new_v4(),
            active_phrase: None,
            phrase_scroll: ScrollHandle::new(),
            navigation:None,
            inputs: BTreeMap::new(),
            areas: BTreeMap::new(),
            subscriptions: vec![],
            readonly,
            compact_marker_card: false,
            dictionary,
            validation_errors: vec![],
            validation_revision: None,
            compact_context: false,
            diagnoses: Default::default(),
        }
    }

    fn phrase_target_visible(&self, target: &PhraseTarget) -> bool {
        if let Some(mark) = self.focus_mark {
            return target.mark_id == Some(mark);
        }
        if let Some(mark) = target.mark_id {
            let catalog_id = self.value["colpo"]["marks"][mark.to_string()]["catalog_id"].as_str();
            let Some(preset) = catalog_id.and_then(m53_domain::colpo::preset) else { return false; };
            let column = usize::from(preset.region != m53_domain::protocol::Region::Cervix);
            return self.expanded_mark == Some(mark) && self.colpo_expanded[column] == Some(preset.group);
        }
        match target.field_code.as_str() {
            "anamnesis" => self.anamnesis_expanded,
            "conclusion" | "diagnosis_note" | "vulva.adequacy_reason" | "anus.adequacy_reason" => true,
            "extended.clock" | "extended.other" | "cervix.adequacy_reason" => self.colpo_expanded[0] == Some(2),
            "rectovaginal.description" => self.colpo_expanded[0] == Some(3),
            "vulva.other_comment" | "vulva.cancer_comment" => self.colpo_expanded[1] == Some(0),
            _ => false,
        }
    }

    fn valid_phrase_target(&self, target: &PhraseTarget) -> bool {
        !self.readonly
            && phrase_target(&self.value, &target.path, self.phrase_form_id).as_ref() == Some(target)
            && self.phrase_target_visible(target)
            && (self.inputs.contains_key(&target.path) || self.areas.contains_key(&target.path))
    }

    /// Keep the last text destination while a template/list control has focus.
    pub fn active_phrase_target(&self) -> Option<PhraseTarget> {
        self.active_phrase.as_ref().filter(|target| self.valid_phrase_target(target)).cloned()
    }

    /// Return keyboard focus from the phrase panel to an existing editable field.
    #[allow(dead_code)]
    pub fn focus_phrase_field(
        &mut self,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let target = phrase_target(&self.value, path, self.phrase_form_id)
            .filter(|target| self.valid_phrase_target(target))
            .ok_or_else(|| "Выбранное текстовое поле больше недоступно".to_string())?;
        let focus = if let Some(state) = self.areas.get(&target.path) {
            state.read(cx).focus_handle(cx)
        } else if let Some(state) = self.inputs.get(&target.path) {
            state.read(cx).focus_handle(cx)
        } else {
            return Err("Выбранное текстовое поле больше недоступно".into());
        };
        focus.focus(window, cx);
        if target.field_code == "conclusion" {
            self.phrase_scroll.scroll_to_bottom();
            cx.notify();
        }
        Ok(())
    }

    fn phrase_focus(&mut self, path: &str, cx: &mut Context<Self>) {
        self.active_phrase = if self.readonly { None } else {
            phrase_target(&self.value, path, self.phrase_form_id)
        };
        cx.notify();
    }

    /// Apply to the captured destination, not whichever input the panel focuses next.
    /// `replace_all` records this replacement as one atomic native Undo operation.
    pub fn apply_phrase(
        &mut self,
        target: &PhraseTarget,
        text: &str,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PhraseApplyResult, String> {
        if !self.valid_phrase_target(target) {
            return Err(if self.readonly {
                "Утвержденный протокол доступен только для чтения".into()
            } else {
                "Выбранное текстовое поле больше недоступно. Выберите его снова".into()
            });
        }
        if self.active_phrase_target().as_ref() != Some(target) {
            return Err("Активное поле изменилось; выберите шаблон для текущего поля".into());
        }
        let current = if let Some(state) = self.areas.get(&target.path) {
            state.read(cx).value().to_string()
        } else if let Some(state) = self.inputs.get(&target.path) {
            state.read(cx).value().to_string()
        } else {
            return Err("Выбранное текстовое поле больше недоступно".into());
        };
        let next = phrase_text(&current, text, replace, target.max_bytes)?;
        let changed = next != current;
        if changed {
            // Persist immediately; the subsequent native Change notification is a no-op.
            self.text_change(&target.path, next.clone(), false, false, matches!(target.field_code.as_str(), "conclusion" | "anamnesis" | "diagnosis_note"), cx);
            if let Some(state) = self.areas.get(&target.path).cloned() {
                state.update(cx, |state, cx| state.replace_all(next.clone(), window, cx));
            } else if let Some(state) = self.inputs.get(&target.path).cloned() {
                state.update(cx, |state, cx| state.replace_all(next.clone(), window, cx));
            }
        }
        Ok(PhraseApplyResult { changed, field_code: target.field_code.clone(), mark_id: target.mark_id })
    }
    fn field_problem(&self, path: &str) -> Option<String> {
        self.validation_errors
            .iter()
            .find(|(target, _)| path == target || path.starts_with(&format!("{target}/")))
            .map(|(_, message)| message.clone())
    }
    fn validation_frame(&self, path: &str, control: AnyElement, cx: &Context<Self>) -> AnyElement {
        let control=if let Some(nav)=self.navigation.as_ref().filter(|n|n.path==path) {
            div().id(SharedString::from(format!("required-focus-{path}")))
                .w_full().anchor_scroll(Some(nav.anchor.clone())).track_focus(&nav.focus)
                .child(control).into_any_element()
        } else {control};
        if let Some(message) = self.field_problem(path) {
            v_flex()
                .w_full()
                .gap_1()
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().danger)
                .child(control)
                .child(div().text_sm().text_color(cx.theme().danger).child(message))
                .into_any_element()
        } else {
            control
        }
    }
    fn put(&mut self, path: &str, value: Value, structural: bool, cx: &mut Context<Self>) {
        if self.readonly {
            return;
        }
        if (path.contains("/adequacy/") || path.contains("/transformation_zone/"))
            && value["state"] == "assessed"
        {
            let index = path
                .strip_prefix("/sections/")
                .and_then(|p| p.split('/').next())
                .and_then(|p| p.parse::<usize>().ok())
                .unwrap_or_else(|| {
                    self.value["sections"]
                        .as_array()
                        .and_then(|sections| sections.iter().position(|s| s["region"] == "cervix"))
                        .unwrap_or(0)
                });
            self.value["sections"][index]["state"] =
                json!({"value":"examined","origin":{"kind":"physician"}});
        }
        if path.starts_with("/colpo/") && path.ends_with("/value") {
            let answer_path = path.strip_suffix("/value").unwrap();
            let Some((parent, key)) = answer_path.rsplit_once('/') else {
                return;
            };
            let Some(map) = self
                .value
                .pointer_mut(parent)
                .and_then(Value::as_object_mut)
            else {
                return;
            };
            map.insert(key.into(), json!({"kind":"text","value":value}));
        } else if path.starts_with("/colpo/") {
            let Some((parent, key)) = path.rsplit_once('/') else {
                return;
            };
            let Some(map) = self
                .value
                .pointer_mut(parent)
                .and_then(Value::as_object_mut)
            else {
                return;
            };
            if value.is_null() {
                map.remove(key);
            } else {
                map.insert(key.into(), value);
            }
        } else if schema::set(&mut self.value, path, value).is_err() {
            return;
        }
        self.audit_fields.insert(path.to_owned());
        self.unfinished.retain(|k, _| !k.starts_with(path));
        if structural {
            self.inputs.clear();
            self.areas.clear();
            self.subscriptions.clear();
            self.active_phrase = None;
        }
        self.generation += 1;
        self.changed = Instant::now();
        cx.notify();
    }
    fn text_change(
        &mut self,
        path: &str,
        text: String,
        number: bool,
        integer: bool,
        assessment: bool,
        cx: &mut Context<Self>,
    ) {
        if self.readonly {
            return;
        }
        // Optional legacy protocols stay byte-compatible until the doctor edits
        // this field. Only then materialize its ordinary physician-origin Field.
        if !prepare_text_edit(&mut self.value, path, &text) { return; }
        if number {
            let parsed = if integer {
                text.parse::<i64>().ok().map(|v| json!(v))
            } else {
                text.replace(',', ".")
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(|v| json!(v))
            };
            if let Some(value) = parsed {
                self.put(path, value, false, cx);
            } else {
                self.audit_fields.insert(path.to_owned());
                self.unfinished.insert(path.into(), text);
                self.generation += 1;
                self.changed = Instant::now();
                cx.notify();
            }
        } else {
            let value = if assessment {
                if text.trim().is_empty() {
                    json!({"state":"not_assessed"})
                } else {
                    json!({"state":"assessed","value":text})
                }
            } else {
                json!(text)
            };
            if self.value.pointer(path) == Some(&value) && !self.unfinished.contains_key(path) {
                return;
            }
            self.put(path, value, false, cx);
        }
    }
    // Schema interpreter carries field kind and GPUI contexts explicitly.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        path: &str,
        value: &Value,
        number: bool,
        integer: bool,
        assessment: bool,
        multiline: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = self.unfinished.get(path).cloned().unwrap_or_else(|| {
            if assessment {
                value["value"].as_str().unwrap_or("").into()
            } else {
                value.as_str().map(str::to_owned).unwrap_or_else(|| {
                    if value.is_null() {
                        String::new()
                    } else {
                        value.to_string()
                    }
                })
            }
        });
        let key = path.to_string();
        let control = if multiline {
            if !self.areas.contains_key(path) {
                let state = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .auto_grow(2, 15)
                        .default_value(text)
                });
                let changed_path = key.clone();
                self.subscriptions.push(cx.subscribe_in(
                    &state,
                    window,
                    move |s, state, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Focus) {
                            s.phrase_focus(&changed_path, cx);
                        }
                        if matches!(event, InputEvent::Change) {
                            s.text_change(
                                &changed_path,
                                state.read(cx).value().to_string(),
                                number,
                                integer,
                                assessment,
                                cx,
                            );
                        }
                    },
                ));
                self.areas.insert(key.clone(), state);
            }
            Textarea::new(self.areas.get(&key).unwrap())
                .readonly(self.readonly)
                .into_any_element()
        } else {
            if !self.inputs.contains_key(path) {
                let state = cx.new(|cx| InputState::new(window, cx).default_value(text));
                let changed_path = key.clone();
                self.subscriptions.push(cx.subscribe_in(
                    &state,
                    window,
                    move |s, state, event: &InputEvent, _, cx| {
                        if matches!(event, InputEvent::Focus) {
                            s.phrase_focus(&changed_path, cx);
                        }
                        if matches!(event, InputEvent::Change) {
                            s.text_change(
                                &changed_path,
                                state.read(cx).value().to_string(),
                                number,
                                integer,
                                assessment,
                                cx,
                            );
                        }
                    },
                ));
                self.inputs.insert(key.clone(), state);
            }
            Input::new(self.inputs.get(&key).unwrap())
                .readonly(self.readonly)
                .into_any_element()
        };
        let control = if !number && self.active_phrase_target().is_some_and(|target| target.path == path) {
            div()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().primary)
                .bg(cx.theme().secondary)
                .child(control)
                .into_any_element()
        } else {
            control
        };
        self.validation_frame(path, control, cx)
    }
    fn choices(
        &self,
        path: &str,
        field_label: &str,
        current: &Value,
        options: Vec<(String, Value)>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let caption = options
            .iter()
            .find(|(_, v)| {
                v == current
                    || (v.is_object()
                        && current.is_object()
                        && ((v.get("state").is_some()
                            && v.get("state") == current.get("state")
                            && v.get("value").is_none_or(|nested| {
                                nested.is_object()
                                    && nested.get("state").is_some()
                                    && nested.get("state")
                                        == current.get("value").and_then(|v| v.get("state"))
                            }))
                            || (v.get("region").is_some()
                                && v.get("region") == current.get("region"))))
            })
            .map(|(label, _)| label.clone())
            .unwrap_or_else(|| "Выберите значение".into());
        if self.readonly {
            return self.validation_frame(path, readonly_value(caption, cx).into_any_element(), cx);
        }
        let entity = cx.entity();
        let field_path = path.to_string();
        let control = Button::new(SharedString::from(format!("choice-{path}")))
            .outline()
            .small()
            .w_full()
            .justify_start()
            .accessibility_label(ergonomics::choice_name(field_label, &caption))
            .tooltip("Выбрать из списка")
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().min_w_0().text_left().child(caption))
                    .child(
                        div()
                            .flex_shrink_0()
                            .pl_2()
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .text_color(cx.theme().foreground)
                            .child(gpui_kit::component::Icon::new(IconName::ChevronDown).small()),
                    ),
            )
            .disabled(self.readonly)
            .dropdown_menu(move |mut menu, _, _| {
                for (label, value) in options.clone() {
                    let entity = entity.clone();
                    let path = field_path.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        entity.update(cx, |s, cx| s.put(&path, value.clone(), true, cx));
                    }));
                }
                menu.max_h(px(15. * 26. + 8.)).scrollable(true)
            })
            .into_any_element();
        self.validation_frame(path, control, cx)
    }
}

impl Render for NativeForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_validation();
        if let Some(id) = self.focus_mark {
            if self.compact_marker_card { return self.marker_properties_card(id, window, cx); }
            return self.mark_properties(id, window, cx);
        }
        let view=self.colpo_view(window, cx);
        if let Some(nav)=self.navigation.as_mut().filter(|n|n.pending) {
            nav.pending=false;
            // Focus the heading, never the first invalid control. One anchor
            // scroll after layout; no second offset/focus-next correction.
            nav.focus.focus(window,cx);
            nav.anchor.scroll_to(window,cx);
        }
        view
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::NativeForm;
    use gpui_kit::{AppContext, TestAppContext};
    use m53_domain::validation::Dictionary;
    use serde_json::json;
    use std::collections::BTreeMap;
    #[gpui_kit::test]
    fn incomplete_numeric_input_survives_without_sending_the_previous_value(
        cx: &mut TestAppContext,
    ) {
        let form = cx.new(|_| {
            NativeForm::new(
                json!({"n":1.5}),
                Dictionary {
                    version_id: uuid::Uuid::new_v4(),
                    clinically_approved: false,
                    terms: vec![],
                },
                BTreeMap::new(),
                false,
            )
        });
        form.update(cx, |form, cx| {
            form.text_change("/n", "-".into(), true, false, false, cx);
            assert_eq!(form.value["n"], 1.5);
            assert_eq!(form.unfinished.get("/n").map(String::as_str), Some("-"));
            form.text_change("/n", "2,75".into(), true, false, false, cx);
            assert_eq!(form.value["n"], 2.75);
            assert!(form.unfinished.is_empty());
        });
    }
}

impl NativeForm {
    fn colpo_field(
        &mut self,
        f: &'static m53_domain::colpo::Field,
        mark: Option<uuid::Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use m53_domain::colpo::Control;
        if mark.is_none() && f.id == "extended.adequacy" {
            return self.adequacy_control(
                Region::Cervix,
                "Кольпоскопическая картина · адекватность оценки шейки матки",
                window,
                cx,
            );
        }
        if mark.is_none() && f.id == "extended.tz" {
            let path = "/cervical_assessment/transformation_zone/value";
            let value = self.value.pointer(path).cloned().unwrap_or(Value::Null);
            let options = vec![
                ("Не оценено".into(), json!({"state":"not_assessed"})),
                ("Не применимо".into(), json!({"state":"not_applicable"})),
                ("ЗТ I".into(), json!({"state":"assessed","value":"type1"})),
                ("ЗТ II".into(), json!({"state":"assessed","value":"type2"})),
                ("ЗТ III".into(), json!({"state":"assessed","value":"type3"})),
            ];
            return v_flex()
                .gap_1()
                .child(f.label)
                .child(self.choices(path, f.label, &value, options, cx))
                .into_any_element();
        }
        let path = mark
            .map(|id| format!("/colpo/marks/{id}/properties/{}", f.id))
            .unwrap_or_else(|| format!("/colpo/fields/{}", f.id));
        let value = self.value.pointer(&path).cloned().unwrap_or(Value::Null);

        match f.control {
            Control::Check => {
                let p = path.clone();
                Checkbox::new(SharedString::from(format!("field-{path}")))
                    .label(f.label)
                    .checked(value["value"].as_bool().unwrap_or(false))
                    .disabled(self.readonly)
                    .on_click(cx.listener(move |s, b, _, cx| {
                        s.put(&p, json!({"kind":"checked","value":b}), false, cx)
                    }))
                    .into_any_element()
            }
            Control::Choice if f.id == "context.phase" => {
                use gpui_kit::component::radio::{Radio, RadioGroup};
                let current = f.choices.iter().position(|v| value["value"] == *v);
                let p = path.clone();
                let clear = path.clone();
                div().flex().flex_wrap().when(self.compact_context, |d| d.flex_col())
                    .gap_2()
                    .when(!self.compact_context, |d| d.items_center())
                    .child(div().when(!self.compact_context, |d| d.w(px(230.)).flex_shrink_0()).child(f.label))
                    .child(
                        h_flex()
                            .w(px(250.))
                            .gap_2()
                            .child(
                                RadioGroup::horizontal("cycle-phase")
                                    .children(
                                        f.choices
                                            .iter()
                                            .enumerate()
                                            .map(|(i, v)| Radio::new(("cycle", i)).label(*v)),
                                    )
                                    .selected_index(current)
                                    .disabled(self.readonly)
                                    .on_change(cx.listener(move |s, i, _, cx| {
                                        s.put(
                                            &p,
                                            json!({"kind":"choice","value":f.choices[*i]}),
                                            false,
                                            cx,
                                        )
                                    })),
                            )
                            .child(
                                Button::new("clear-phase")
                                    .ghost()
                                    .small()
                                    .icon(IconName::X)
                                    .tooltip("Сбросить фазу цикла")
                                    .accessibility_label("Сбросить фазу цикла")
                                    .disabled(self.readonly)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.put(&clear, Value::Null, false, cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            Control::Choice => {
                let mut options = vec![("Не заполнено".into(), Value::Null)];
                options.extend(f.choices.iter().map(|v| {
                    (
                        match *v {
                            "БледноРозовая" => "Бледно-розовая",
                            "НеВидна" => "Не видна",
                            _ => v,
                        }
                        .to_string(),
                        json!({"kind":"choice","value":v}),
                    )
                }));
                v_flex()
                    .gap_1()
                    .child(f.label)
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(self.choices(&path, f.label, &value, options, cx)),
                            )
                            .child(
                                Button::new(SharedString::from(format!("clear-{path}")))
                                    .ghost()
                                    .small()
                                    .icon(IconName::X)
                                    .tooltip("Сбросить поле")
                                    .accessibility_label(format!("Сбросить поле: {}", f.label))
                                    .disabled(self.readonly)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.put(&path, Value::Null, false, cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            Control::Text => {
                let value = self.value.pointer(&path).cloned().unwrap_or(Value::Null);
                let input = self.text(
                    &format!("{path}/value"),
                    &value["value"],
                    false,
                    false,
                    false,
                    true,
                    window,
                    cx,
                );
                v_flex()
                    .gap_1()
                    .child(f.label)
                    .when(matches!(f.id, "extended.other" | "extended.clock" | "rectovaginal.description"),
                        |d| d.child(div().text_xs().text_color(cx.theme().muted_foreground).child("По ситуации")))
                    .child(input)
                    .into_any_element()
            }
            Control::Date => {
                use gpui_kit::component::{
                    calendar::Date,
                    date_picker::{DatePicker, DatePickerEvent, DateTime},
                };
                if !self.dates.contains_key(&path) {
                    let initial = value["value"].as_str().unwrap_or("").to_string();
                    let state = cx.new(|cx| {
                        let mut state = m53_gpui_editor::calendar_locale::date_state(window, cx);
                        if let Ok(day) = initial.parse() {
                            state.set_date(Date::Single(Some(day)), window, cx);
                        }
                        state
                    });
                    let p = path.clone();
                    self.subscriptions
                        .push(cx.subscribe(&state, move |s, _, event, cx| {
                            let DatePickerEvent::Change(value) = event;
                            let value = match value {
                                DateTime::Single(Some(v)) => {
                                    json!({"kind":"text","value":v.format("%Y-%m-%d").to_string()})
                                }
                                _ => Value::Null,
                            };
                            s.put(&p, value, false, cx);
                        }));
                    self.dates.insert(path.clone(), state);
                }
                let date_control = if self.readonly {
                    let stored = value["value"].as_str().unwrap_or("");
                    let label = if stored.is_empty() { "Не выбрана".to_owned() } else {
                        chrono::NaiveDate::parse_from_str(stored, "%Y-%m-%d")
                            .map(|date| date.format("%d.%m.%Y").to_string())
                            .unwrap_or_else(|_| stored.to_owned())
                    };
                    readonly_value(label, cx).into_any_element()
                } else {
                    DatePicker::new(self.dates.get(&path).unwrap())
                        .small().placeholder("Не выбрана").into_any_element()
                };
                div().flex()
                    .when(self.compact_context, |d| d.flex_col().w_full())
                    .when(!self.compact_context, |d| d.w(px(428.)).flex_shrink_0().items_center())
                    .gap_2()
                    .child(div().when(!self.compact_context, |d| d.w(px(230.)).flex_shrink_0()).child(f.label))
                    .child(
                        div().w(px(190.)).child(date_control),
                    )
                    .into_any_element()
            }
            Control::Unknown => v_flex()
                .child(f.label)
                .child("Варианты не уточнены")
                .into_any_element(),
        }
    }
    pub fn mark_properties(
        &mut self,
        id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mark = self.value["colpo"]["marks"][id.to_string()].clone();
        let Some(p) = m53_domain::colpo::preset(mark["catalog_id"].as_str().unwrap_or("")) else {
            return div().into_any_element();
        };
        let mut out = v_flex().gap_3().child(
            div()
                .font_semibold()
                .child(format!("{} · {}", p.short, p.name)),
        );
        let fields: Vec<_> = m53_domain::colpo::FIELDS
            .iter()
            .filter(|f| f.parent == p.id)
            .collect();
        if fields.is_empty() {
            out = out.child(div().text_sm().child("Дополнительных характеристик нет"));
        }
        for f in fields {
            out = out.child(self.colpo_field(f, Some(id), window, cx));
        }
        out.into_any_element()
    }
    /// Uses the same field states and edits as the protocol; the canvas owns the
    /// anchored card heading, bounds and its single outer scroll container.
    pub fn marker_properties_card(
        &mut self,
        id: uuid::Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mark = self.value["colpo"]["marks"][id.to_string()].clone();
        let Some(preset) = m53_domain::colpo::preset(mark["catalog_id"].as_str().unwrap_or("")) else {
            return div().into_any_element();
        };
        let fields: Vec<_> = m53_domain::colpo::FIELDS.iter().filter(|f| f.parent == preset.id).collect();
        let mut out = v_flex().gap_2().w_full().min_w_0();
        if fields.is_empty() { out = out.child(div().text_sm().child("Дополнительных характеристик нет")); }
        for field in fields { out = out.child(self.colpo_field(field, Some(id), window, cx)); }
        out.into_any_element()
    }
    #[cfg(not(windows))]
    pub fn colpo_group_for_test(&self, column: usize) -> Option<usize> {
        self.colpo_expanded[column]
    }
    #[cfg(not(windows))]
    pub fn navigation_for_test(&self)->Option<(String,FocusHandle)> {
        self.navigation.as_ref().map(|n|(n.path.clone(),n.focus.clone()))
    }
    #[cfg(not(windows))]
    pub fn scroll_for_test(&self)->ScrollHandle {self.phrase_scroll.clone()}
    fn cervix_removed_switch(&self, cx: &mut Context<Self>) -> AnyElement {
        gpui_kit::component::switch::Switch::new("cervix-removed")
            .label("Удалена")
            .checked(self.value["colpo"]["fields"]["context.no_cervix"]["value"] == true)
            .disabled(self.readonly)
            .on_change(cx.listener(|s, value, _, cx| {
                s.active_phrase = None;
                s.put("/colpo/fields/context.no_cervix", json!({"kind":"checked","value":*value}), true, cx);
            }))
            .into_any_element()
    }
    fn problem_group(&self, path: &str) -> Option<(usize, usize)> {
        if let Some(code) = path.strip_prefix("/colpo/fields/").and_then(|p| p.split('/').next()) {
            let field = m53_domain::colpo::FIELDS.iter().find(|f| f.id == code && f.parent.is_empty())?;
            if field.group >= 4 { return None; }
            return Some((usize::from(field.id.starts_with("vulva.")), field.group));
        }
        if let Some(id) = path.strip_prefix("/colpo/marks/").and_then(|p| p.split('/').next()) {
            let preset = m53_domain::colpo::preset(self.value["colpo"]["marks"][id]["catalog_id"].as_str()?)?;
            return Some((usize::from(preset.region != m53_domain::protocol::Region::Cervix), preset.group));
        }
        if path.starts_with("/cervical_assessment/") { return Some((0, 2)); }
        if let Some(index) = path.strip_prefix("/sections/").and_then(|p| p.split('/').next())
            .and_then(|i| i.parse::<usize>().ok()) {
            if self.value["sections"][index]["region"] == "cervix" { return Some((0, 2)); }
        }
        None
    }

    fn colpo_group_problem_count(&self, column: usize, group: usize) -> usize {
        self.validation_errors.iter()
            .filter(|(path, _)| self.problem_group(path) == Some((column, group)))
            .map(|(path, _)| path)
            .collect::<std::collections::BTreeSet<_>>().len()
    }

    fn group_has_input(&self, column: usize, group: usize) -> bool {
        fn entered(value: &Value) -> bool {
            match value {
                Value::Null => false,
                Value::Bool(v) => *v,
                Value::String(v) => !v.trim().is_empty(),
                Value::Array(v) => v.iter().any(entered),
                Value::Object(v) => {
                    if let Some(state) = v.get("state").and_then(Value::as_str) {
                        return !matches!(state, "not_assessed" | "not_examined");
                    }
                    v.get("value").is_some_and(entered)
                }
                Value::Number(_) => true,
            }
        }
        let fields = m53_domain::colpo::FIELDS.iter().filter(|f| f.parent.is_empty()
            && f.group == group && usize::from(f.id.starts_with("vulva.")) == column);
        fields.into_iter().any(|f| entered(&self.value["colpo"]["fields"][f.id]))
            || self.value["colpo"]["marks"].as_object().is_some_and(|marks| marks.values().any(|m| {
                m53_domain::colpo::preset(m["catalog_id"].as_str().unwrap_or(""))
                    .is_some_and(|p| p.group == group && usize::from(p.region != m53_domain::protocol::Region::Cervix) == column)
            }))
            || (column == 0 && group == 2 && (entered(&self.value["cervical_assessment"]["transformation_zone"])
                || self.value["sections"].as_array().is_some_and(|sections| sections.iter()
                    .any(|s| s["region"] == "cervix" && entered(&s["adequacy"])))))
    }
    fn colpo_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use m53_domain::colpo::{FIELDS, PRESETS};
        let catalog =
            serde_json::from_value::<m53_domain::colpo::Colpo>(self.value["colpo"].clone()).ok();
        let hidden = ["context.no_cervix", "context.skip"]
            .iter()
            .any(|id| self.value["colpo"]["fields"][id]["value"] == true);
        let mut columns = vec![];
        for column in 0..2usize {
            let mut out = v_flex().gap_2().w_full().min_w_0();
            if column == 0 {
                let fields: Vec<_> = FIELDS
                    .iter()
                    .filter(|f| f.group == 4 && f.parent.is_empty() && f.id != "context.no_cervix")
                    .collect();
                let mut flags = h_flex().gap_4().flex_wrap();
                for f in fields {
                    if matches!(
                        f.id,
                        "context.pregnancy" | "context.menopause"
                    ) {
                        flags = flags.child(self.colpo_field(f, None, window, cx));
                        continue;
                    }
                    if f.id == "context.skip" {
                        out = out.child(flags);
                        flags = h_flex().gap_4().flex_wrap();
                    }
                    let control = self.colpo_field(f, None, window, cx);
                    if f.id != "context.lmp" {
                        out = out.child(control);
                    } else {
                        let colpo = serde_json::from_value::<m53_domain::colpo::Colpo>(
                            self.value["colpo"].clone(),
                        )
                        .ok();
                        let day = colpo.as_ref().and_then(|c| c.cycle_day());
                        let message = if let Some(day) = day {
                            format!("День цикла: {day}")
                        } else if ["context.pregnancy", "context.menopause"]
                            .iter()
                            .any(|id| self.value["colpo"]["fields"][id]["value"] == true)
                        {
                            "День цикла: не применяется".into()
                        } else {
                            "День цикла: — (проверьте обе даты)".into()
                        };
                        out = out.child(
                            h_flex()
                                .gap_3()
                                .items_center()
                                .flex_wrap()
                                .child(control)
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(message),
                                ),
                        );
                    }
                }
            }
            let heading = crate::ui_chrome::section_heading(if column == 0 {
                "Кольпоскопия"
            } else {
                "Оценка вульвы"
            }, cx);
            let heading = if column == 1 {
                if let Some(nav) = self.navigation.as_ref().filter(|n| n.path == "vulva-heading") {
                    div().id("vulva-heading-anchor").anchor_scroll(Some(nav.anchor.clone()))
                        .track_focus(&nav.focus).child(heading).into_any_element()
                } else { heading.into_any_element() }
            } else { heading.into_any_element() };
            out = out.child(heading);
            if column == 1 {
                out = out.child(self.adequacy_control(Region::Vulva, "Адекватность оценки вульвы", window, cx));
                let anal = self.value["colpo"]["marks"].as_object().is_some_and(|m| {
                    m.values()
                        .any(|v| v["catalog_id"] == "vulva.magnification.analjunction")
                });
                if anal {
                    out = out.child(self.adequacy_control(
                        Region::Anus,
                        "Адекватность оценки анальной области",
                        window,
                        cx,
                    ));
                }
            }
            let groups = if column == 0 {
                vec![
                    "Шейка матки",
                    "Простая кольпоскопия",
                    "Расширенная кольпоскопия",
                    "Ректо-вагинальный осмотр",
                ]
            } else {
                vec![
                    "Клиническая терминология",
                    "Кольпоскопическая терминология поражений вульвы",
                ]
            };
            if column == 0 && hidden {
                out = out
                    .child(h_flex().gap_3().items_center()
                        .child(div().flex_1().font_semibold().child("Шейка матки"))
                        .child(self.cervix_removed_switch(cx)))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(
                        if self.value["colpo"]["fields"]["context.no_cervix"]["value"] == true {
                            "Шейка матки удалена. Поля шейки не заполняются и не требуются для утверждения."
                        } else { "Данные кольпоскопии не заполняются." }))
                    .child(div().text_sm().text_color(cx.theme().muted_foreground)
                        .child("Ранее введенные данные сохранены и вернутся после выключения переключателя."));
            } else {
                for (group, title) in groups.into_iter().enumerate() {
                    let open = self.colpo_expanded[column] == Some(group);
                    let missing = self.colpo_group_problem_count(column, group);
                    let needs_attention = missing > 0;
                    let summary = ergonomics::section_summary(missing, self.group_has_input(column, group), column == 0 && group == 3);
                    let header = Button::new(SharedString::from(format!("colpo-group-{column}-{group}")))
                            .outline()
                            .small()
                            .selected(open)
                            .w_full()
                            .justify_start()
                            .accessibility_label(title)
                            .when(needs_attention, |b| {
                                b.border_color(cx.theme().danger)
                                    .text_color(cx.theme().danger)
                            })
                            .accessibility_label(if summary.is_empty() {title.to_string()} else {format!("{title} · {summary}")})
                            .child(div().flex_1().min_w_0().text_left().child(title.to_string()))
                            .when(!summary.is_empty(), |b| b.child(div().text_xs().child(summary)))
                            .icon(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .on_click(cx.listener(move |s, _, _, cx| {
                                let next = if s.colpo_expanded[column] == Some(group) {
                                    None
                                } else {
                                    Some(group)
                                };
                                s.colpo_expanded = [None, None];
                                s.active_phrase = None;
                                if next.is_some() {s.audit_sections.push(format!("colpo.section.{column}.{group}"));}
                                s.colpo_expanded[column] = next;
                                s.expanded_mark = None;
                                if next.is_some() {s.navigate_group(column,group,cx);}
                                else {s.navigation=None;}
                                cx.notify();
                            }));
                    let group_path=format!("group:{column}:{group}");
                    let header=if let Some(nav)=self.navigation.as_ref().filter(|n|n.path==group_path) {
                        div().id(SharedString::from(group_path)).flex_1().min_w_0()
                            .anchor_scroll(Some(nav.anchor.clone())).track_focus(&nav.focus)
                            .child(header).into_any_element()
                    } else {header.into_any_element()};
                    if column == 0 && group == 0 {
                        out = out.child(h_flex().w_full().gap_3().items_center()
                            .child(div().flex_1().min_w_0().child(header))
                            .child(self.cervix_removed_switch(cx)));
                    } else { out = out.child(header); }
                    if !open {
                        continue;
                    }
                    let mut contents = v_flex()
                        .gap_2()
                        .p_2()
                        .border_l_1()
                        .border_b_1()
                        .border_color(cx.theme().border);
                    if column == 0 && group == 2 {
                        contents = contents.child(protocol_heading("Общие положения", cx));
                    }
                    if column == 0 {
                        let mut fields: Vec<_> = FIELDS
                            .iter()
                            .filter(|f| {
                                f.group == group
                                    && f.parent.is_empty()
                                    && !(group == 0
                                        && matches!(f.id, "cervix.surface" | "cervix.color"))
                                    && !(group == 2
                                        && matches!(
                                            f.id,
                                            "extended.location"
                                                | "extended.clock"
                                                | "extended.size"
                                                | "extended.other"
                                        ))
                            })
                            .collect();
                        if group == 1 {
                            let order = [
                                "simple.relief",
                                "simple.vessels",
                                "simple.closed",
                                "simple.open",
                                "simple.closed_count",
                                "simple.open_count",
                                "simple.transition",
                            ];
                            fields.sort_by_key(|f| {
                                order.iter().position(|id| *id == f.id).unwrap_or(100)
                            });
                        }
                        for f in fields {
                            contents = contents.child(self.colpo_field(f, None, window, cx));
                        }
                    }
                    let mut symbols: Vec<_> = PRESETS
                        .iter()
                        .filter(|p| {
                            ((p.region == m53_domain::protocol::Region::Cervix) == (column == 0))
                                && p.group == group
                        })
                        .collect();
                    symbols.sort_by_key(|p| m53_domain::colpo::subgroup(p).1);
                    if !symbols.is_empty() {
                        contents = contents.child(div().text_xs().text_color(cx.theme().muted_foreground)
                            .child("По меткам на снимках"));
                    }
                    let mut previous = None;
                    for p in symbols {
                        let section = m53_domain::colpo::subgroup(p);
                        if previous != Some(section) && !(column == 0 && group == 0) {
                            if column == 0 && group == 2 && section.1 == 1 {
                                contents = contents
                                    .child(protocol_heading(
                                        "Аномальные кольпоскопические картины",
                                        cx,
                                    ))
                                    .child(div().font_semibold().child("Общие принципы"));
                                for id in ["extended.location", "extended.clock", "extended.size"] {
                                    let f = FIELDS.iter().find(|f| f.id == id).unwrap();
                                    contents =
                                        contents.child(self.colpo_field(f, None, window, cx));
                                }
                            }
                            if column == 1 && group == 0 && section.1 == 2 {
                                contents = contents.child(protocol_heading("Цвет кожи", cx));
                                for id in ["vulva.skin.red", "vulva.skin.white", "vulva.skin.dark"]
                                {
                                    let f = FIELDS.iter().find(|f| f.id == id).unwrap();
                                    contents =
                                        contents.child(self.colpo_field(f, None, window, cx));
                                }
                            }
                            if column == 1 && group == 0 && section.1 == 4 {
                                let f = FIELDS
                                    .iter()
                                    .find(|f| f.id == "vulva.other_comment")
                                    .unwrap();
                                contents = contents.child(self.colpo_field(f, None, window, cx));
                            }
                            contents = contents.child(protocol_heading(section.0, cx));
                            previous = Some(section);
                        }
                        let marks: Vec<_> = self.value["colpo"]["marks"]
                            .as_object()
                            .into_iter()
                            .flat_map(|m| m.iter())
                            .filter(|(_, m)| m["catalog_id"] == p.id)
                            .map(|(id, m)| (id.clone(), m.clone()))
                            .collect();
                        contents = contents.child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    Checkbox::new(SharedString::from(format!("symbol-{}", p.id)))
                                        .checked(!marks.is_empty())
                                        .accessibility_label(format!(
                                            "{} — {}: по меткам на снимках",
                                            p.short, p.name
                                        ))
                                        .disabled(true),
                                )
                                .child(
                                    div()
                                        .font_semibold()
                                        .text_color(crate::appearance::marker_text(
                                            p.marker_color(),
                                            cx,
                                        ))
                                        .child(p.short),
                                )
                                .child(div().text_color(cx.theme().foreground).child(p.name)),
                        );
                        let properties: Vec<_> =
                            FIELDS.iter().filter(|f| f.parent == p.id).collect();
                        let mut links = h_flex().gap_2().flex_wrap();
                        let mut any_links = false;
                        for (number, (id, mark)) in marks.into_iter().enumerate() {
                            let Ok(id) = id.parse::<uuid::Uuid>() else {
                                continue;
                            };
                            let photo = mark["asset_id"]
                                .as_str()
                                .and_then(|id| id.parse::<uuid::Uuid>().ok())
                                .and_then(|id| self.photo_order.iter().position(|p| *p == id))
                                .map(|i| format!("Снимок {} · ", i + 1))
                                .unwrap_or_default();
                            let ordinal = catalog
                                .as_ref()
                                .and_then(|c| m53_client::report::mark_number(c, id))
                                .unwrap_or(number + 1);
                            let label = format!("{photo}Метка {ordinal}");
                            let show = Button::new(SharedString::from(format!("show-mark-{id}")))
                                .outline()
                                .small()
                                .icon(IconName::Crosshair)
                                .tooltip("Показать метку на снимке")
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.show_mark = Some(id);
                                    cx.notify();
                                }));
                            if properties.is_empty() {
                                links = links.child(show.label(label));
                                any_links = true;
                                continue;
                            }
                            let open = self.expanded_mark == Some(id);
                            let mut block = v_flex()
                                .gap_2()
                                .pl_4()
                                .border_l_1()
                                .border_color(cx.theme().border)
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Button::new(SharedString::from(format!(
                                                "mark-properties-{id}"
                                            )))
                                            .outline()
                                            .small()
                                            .selected(open)
                                            .justify_start()
                                            .icon(if open {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            })
                                            .label(format!("{label} · Характеристики"))
                                            .on_click(cx.listener(move |s, _, _, cx| {
                                                s.active_phrase = None;
                                                s.expanded_mark = if s.expanded_mark == Some(id) {
                                                    None
                                                } else {
                                                    Some(id)
                                                };
                                                cx.notify();
                                            })),
                                        )
                                        .child(show),
                                );
                            if open {
                                for f in &properties {
                                    block = block.child(self.colpo_field(f, Some(id), window, cx));
                                }
                            }
                            contents = contents.child(block);
                        }
                        if any_links {
                            contents = contents.child(div().pl_4().child(links));
                        }
                    }
                    if column == 0 && group == 2 {
                        let f = FIELDS.iter().find(|f| f.id == "extended.other").unwrap();
                        contents = contents.child(self.colpo_field(f, None, window, cx));
                    }
                    if column == 0 && group == 0 {
                        contents = contents.child(protocol_heading("Слизистая", cx));
                        for id in ["cervix.surface", "cervix.color"] {
                            let f = FIELDS.iter().find(|f| f.id == id).unwrap();
                            contents = contents.child(self.colpo_field(f, None, window, cx));
                        }
                    }
                    if column == 1 && group == 0 {
                        let f = FIELDS
                            .iter()
                            .find(|f| f.id == "vulva.cancer_comment")
                            .unwrap();
                        contents = contents.child(self.colpo_field(f, None, window, cx));
                    }
                    out = out.child(
                        div()
                            .id(SharedString::from(format!("colpo-scroll-{column}-{group}")))
                            .w_full()
                            .child(contents),
                    );
                }
            }
            columns.push(
                div()
                    .id(("colpo-column", column))
                    .w_full()
                    .min_w_0()
                    .child(out),
            );
        }
        let open = self.anamnesis_expanded;
        let entered = self.value["history"]["value"].as_str().is_some_and(|s| !s.trim().is_empty());
        let summary = ergonomics::section_summary(0, entered, true);
        let header = Button::new("anamnesis-toggle").outline().small().w_full()
            .selected(open).icon(if open {IconName::ChevronDown} else {IconName::ChevronRight})
            .accessibility_label(format!("Анамнез · {summary}"))
            .child(div().flex_1().text_left().child("Анамнез"))
            .child(div().text_xs().child(summary))
            .on_click(cx.listener(|s,_,_,cx| {
                s.anamnesis_expanded = !s.anamnesis_expanded;
                s.active_phrase = None;
                if s.anamnesis_expanded {
                    s.audit_sections.push("colpo.section.anamnesis".to_string());
                    s.navigate_section("anamnesis".to_string(),cx);
                } else {s.navigation=None;}
                cx.notify();
            }));
        let header = if let Some(nav)=self.navigation.as_ref().filter(|n|n.path=="anamnesis") {
            div().id("anamnesis-anchor").anchor_scroll(Some(nav.anchor.clone()))
                .track_focus(&nav.focus).child(header).into_any_element()
        } else {header.into_any_element()};
        let mut history = v_flex().gap_2().child(header);
        if open {
            let input = self.text("/history/value", &self.value["history"]["value"].clone(), false, false, true, true, window, cx);
            history = history.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("Необязательное поле")).child(input);
        }
        let conclusion = self.text(
            "/conclusion/value",
            &self.value["conclusion"]["value"].clone(),
            false,
            false,
            true,
            true,
            window,
            cx,
        );
        let conclusion_heading = crate::ui_chrome::section_heading("Заключение врача", cx);
        let conclusion_heading = if let Some(nav) = self.navigation.as_ref().filter(|n| n.path == "conclusion") {
            div().id("conclusion-heading-anchor").anchor_scroll(Some(nav.anchor.clone()))
                .track_focus(&nav.focus).child(conclusion_heading).into_any_element()
        } else { conclusion_heading.into_any_element() };
        div()
            .id("colpo-protocol-scroll")
            .size_full()
            .track_scroll(&self.phrase_scroll)
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_3()
                    .p_3()
                    .w_full()
                    .max_w(px(1040.))
                    .children(columns)
                    .child(history)
                    .child(self.diagnoses_view(window, cx))
                    .child(
                        v_flex()
                            .gap_1()
                            .py_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(conclusion_heading)
                            .child(conclusion),
                    ),
            )
            .into_any_element()
    }
}

impl NativeForm {
    fn adequacy_control(
        &mut self,
        region: Region,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(path) = adequacy_path(&self.value, region) else {
            return readonly_value("Раздел оценки отсутствует в протоколе", cx).into_any_element();
        };
        let value = self.value.pointer(&path).cloned().unwrap_or(Value::Null);
        let reason = value["value"]["reason"].as_str().unwrap_or("");
        let options = vec![
            ("Не оценено".into(), json!({"state":"not_assessed"})),
            (
                "Адекватная".into(),
                json!({"state":"assessed","value":{"state":"adequate"}}),
            ),
            (
                "Неадекватная".into(),
                json!({"state":"assessed","value":{"state":"inadequate","reason":reason}}),
            ),
        ];
        let mut out = v_flex()
            .gap_1()
            .child(title.to_string())
            .child(self.choices(&path, title, &value, options, cx));
        if value["value"]["state"] == "inadequate" {
            out = out.child("Причина неадекватности").child(self.text(
                &format!("{path}/value/reason"),
                &value["value"]["reason"],
                false,
                false,
                false,
                true,
                window,
                cx,
            ));
        }
        out.into_any_element()
    }
}
