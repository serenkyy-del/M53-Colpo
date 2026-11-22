use crate::model::field;
use eframe::egui;
use m53_domain::{ExaminationState, protocol::*};
use uuid::Uuid;
fn missing<T>() -> Field<Assessment<T>> {
    field(Assessment::NotAssessed)
}
fn enumeration<T: Clone + std::fmt::Debug + PartialEq>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut Field<Assessment<T>>,
    options: &[(T, &str)],
) {
    ui.push_id(label, |ui| {
        ui.label(label);
        let selected = match &value.value {
            Assessment::Assessed(v) => options
                .iter()
                .find(|(option, _)| option == v)
                .map(|(_, s)| s.to_string())
                .unwrap_or_else(|| format!("{v:?}")),
            Assessment::NotAssessed => "Не оценено".into(),
            Assessment::NotApplicable => "Не применимо".into(),
        };
        egui::ComboBox::from_id_salt("value")
            .selected_text(selected)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut value.value, Assessment::NotAssessed, "Не оценено");
                ui.selectable_value(&mut value.value, Assessment::NotApplicable, "Не применимо");
                for (option, label) in options {
                    ui.selectable_value(
                        &mut value.value,
                        Assessment::Assessed(option.clone()),
                        *label,
                    );
                }
            });
    });
}
fn number(ui: &mut egui::Ui, label: &str, value: &mut Field<Assessment<f64>>, max: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut assessed = matches!(value.value, Assessment::Assessed(_));
        if ui.checkbox(&mut assessed, "Оценено").changed() {
            *value = field(if assessed {
                Assessment::Assessed(0.0)
            } else {
                Assessment::NotAssessed
            });
        }
        if let Assessment::Assessed(n) = &mut value.value {
            ui.add(egui::DragValue::new(n).range(0.0..=max).speed(0.1));
        }
    });
}
fn count(ui: &mut egui::Ui, label: &str, value: &mut Field<Assessment<u8>>, min: u8, max: u8) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut assessed = matches!(value.value, Assessment::Assessed(_));
        if ui.checkbox(&mut assessed, "Оценено").changed() {
            *value = field(if assessed {
                Assessment::Assessed(min)
            } else {
                Assessment::NotAssessed
            });
        }
        if let Assessment::Assessed(n) = &mut value.value {
            ui.add(egui::DragValue::new(n).range(min..=max));
        }
    });
}
fn code(ui: &mut egui::Ui, label: &str, value: &mut Field<Assessment<String>>) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut text = if let Assessment::Assessed(s) = &value.value {
            s.clone()
        } else {
            String::new()
        };
        if ui.text_edit_singleline(&mut text).changed() {
            *value = field(if text.trim().is_empty() {
                Assessment::NotAssessed
            } else {
                Assessment::Assessed(text)
            });
        }
    });
}
pub fn cervical(ui: &mut egui::Ui, c: &mut CervicalAssessment) {
    ui.collapsing(
        "Стык и зона трансформации шейки",
        |ui| {
            enumeration(
                ui,
                "Видимость стыка",
                &mut c.junction_visibility,
                &[
                    (JunctionVisibility::FullyVisible, "Полная"),
                    (JunctionVisibility::PartiallyVisible, "Частичная"),
                    (JunctionVisibility::NotVisible, "Не виден"),
                ],
            );
            enumeration(
                ui,
                "Тип зоны трансформации",
                &mut c.transformation_zone,
                &[
                    (CervicalTransformationZone::Type1, "Тип 1"),
                    (CervicalTransformationZone::Type2, "Тип 2"),
                    (CervicalTransformationZone::Type3, "Тип 3"),
                ],
            );
        },
    );
}
pub fn lesions(
    ui: &mut egui::Ui,
    s: &mut RegionSection,
    dictionary: Option<&m53_domain::validation::Dictionary>,
) {
    if s.state.value != ExaminationState::Examined {
        return;
    }
    if ui.button("Добавить отдельный очаг").clicked() {
        s.lesions.push(Lesion {
            id: Uuid::new_v4(),
            location: match s.region {
                Region::Cervix => Location::Cervix {
                    zone_relation: missing(),
                    clock_start: missing(),
                    clock_end: missing(),
                    quadrants: missing(),
                    affected_percent: missing(),
                },
                Region::Vagina => Location::Vagina {
                    thirds: missing(),
                    walls: missing(),
                    zone_relation: missing(),
                },
                Region::Vulva => Location::Vulva {
                    anatomy_code: missing(),
                    tissue: missing(),
                    length_cm: missing(),
                    width_cm: missing(),
                },
                Region::Anus => Location::Anus {
                    anatomy_code: missing(),
                    dentate_line_relation: missing(),
                    length_cm: missing(),
                    width_cm: missing(),
                },
            },
            findings: vec![],
        });
    }
    let mut remove = None;
    for (i, l) in s.lesions.iter_mut().enumerate() {
        ui.push_id(l.id,|ui|{ui.collapsing(format!("Очаг {}",i+1),|ui|{match &mut l.location{
 Location::Cervix{zone_relation,clock_start,clock_end,quadrants,affected_percent}=>{enumeration(ui,"Отношение к зоне",zone_relation,&[(ZoneRelation::Inside,"Внутри"),(ZoneRelation::Outside,"Вне"),(ZoneRelation::Both,"Внутри и вне")]);count(ui,"Часы от",clock_start,1,12);count(ui,"Часы до",clock_end,1,12);count(ui,"Квадранты",quadrants,0,4);number(ui,"Площадь поражения, %",affected_percent,100.0);},
 Location::Vagina{thirds,walls,zone_relation}=>{enumeration(ui,"Отношение к зоне",zone_relation,&[(ZoneRelation::Inside,"Внутри"),(ZoneRelation::Outside,"Вне"),(ZoneRelation::Both,"Внутри и вне")]);ui.label("Треть влагалища");for(value,label)in[(VaginalThird::Upper,"Верхняя"),(VaginalThird::Middle,"Средняя"),(VaginalThird::Lower,"Нижняя")]{let mut selected=matches!(&thirds.value,Assessment::Assessed(values) if values.contains(&value));if ui.checkbox(&mut selected,label).changed(){let mut values=if let Assessment::Assessed(v)=&thirds.value{v.clone()}else{vec![]};values.retain(|v|v!=&value);if selected{values.push(value);}*thirds=field(if values.is_empty(){Assessment::NotAssessed}else{Assessment::Assessed(values)});}}ui.label("Стенка");for(value,label)in[(VaginalWall::Anterior,"Передняя"),(VaginalWall::Posterior,"Задняя"),(VaginalWall::Left,"Левая"),(VaginalWall::Right,"Правая")]{let mut selected=matches!(&walls.value,Assessment::Assessed(values) if values.contains(&value));if ui.checkbox(&mut selected,label).changed(){let mut values=if let Assessment::Assessed(v)=&walls.value{v.clone()}else{vec![]};values.retain(|v|v!=&value);if selected{values.push(value);}*walls=field(if values.is_empty(){Assessment::NotAssessed}else{Assessment::Assessed(values)});}}},
 Location::Vulva{anatomy_code,tissue,length_cm,width_cm}=>{code(ui,"Код анатомической области из словаря",anatomy_code);enumeration(ui,"Ткань",tissue,&[(Tissue::HairBearingSkin,"Кожа с волосами"),(Tissue::NonHairBearingSkin,"Кожа без волос"),(Tissue::Mucosa,"Слизистая")]);number(ui,"Длина, см (ввод врача)",length_cm,f64::MAX);number(ui,"Ширина, см (ввод врача)",width_cm,f64::MAX);},
 Location::Anus{anatomy_code,dentate_line_relation,length_cm,width_cm}=>{code(ui,"Код анатомической области из словаря",anatomy_code);enumeration(ui,"Отношение к зубчатой линии",dentate_line_relation,&[(DentateLineRelation::Above,"Выше"),(DentateLineRelation::Below,"Ниже"),(DentateLineRelation::Crossing,"Пересекает")]);number(ui,"Длина, см (ввод врача)",length_cm,f64::MAX);number(ui,"Ширина, см (ввод врача)",width_cm,f64::MAX);}}
 findings(ui,&mut l.findings,dictionary,s.region);
 if ui.button("Удалить очаг").clicked(){remove=Some(i);}});});
    }
    if let Some(i) = remove {
        s.lesions.remove(i);
    }
}
pub fn excision(ui: &mut egui::Ui, s: &mut ExcisionSection) {
    ui.collapsing("Эксцизия", |ui| {
        egui::ComboBox::from_id_salt("excision-state")
            .selected_text(format!("{:?}", s.state.value))
            .show_ui(ui, |ui| {
                for (value, label) in [
                    (ExaminationState::Examined, "Выполнена"),
                    (ExaminationState::NotExamined, "Не выполнена"),
                    (ExaminationState::NotApplicable, "Не применимо"),
                ] {
                    ui.selectable_value(&mut s.state.value, value, label);
                }
            });
        if s.state.value == ExaminationState::Examined {
            if ui.button("Добавить образец").clicked() {
                s.specimens.push(ExcisionSpecimen {
                    id: Uuid::new_v4(),
                    excision_type: missing(),
                    length_mm: missing(),
                    thickness_mm: missing(),
                    circumference_mm: missing(),
                });
            }
            let mut remove = None;
            for (i, s) in s.specimens.iter_mut().enumerate() {
                ui.push_id(s.id, |ui| {
                    ui.label(format!("Образец {}", i + 1));
                    enumeration(
                        ui,
                        "Тип эксцизии",
                        &mut s.excision_type,
                        &[
                            (ExcisionType::Type1, "Тип 1"),
                            (ExcisionType::Type2, "Тип 2"),
                            (ExcisionType::Type3, "Тип 3"),
                        ],
                    );
                    number(ui, "Длина, мм", &mut s.length_mm, f64::MAX);
                    number(ui, "Толщина, мм", &mut s.thickness_mm, f64::MAX);
                    number(
                        ui,
                        "Окружность, мм (необязательно)",
                        &mut s.circumference_mm,
                        f64::MAX,
                    );
                    if ui.button("Удалить образец").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                s.specimens.remove(i);
            }
        }
    });
}

pub fn clinical_context(ui: &mut egui::Ui, context: &mut Option<ClinicalContext>) -> bool {
    let before = serde_json::to_value(&*context).unwrap();
    ui.collapsing(
        "Цикл, беременность и методика",
        |ui| {
            if context.is_none() && ui.button("Добавить сведения").clicked() {
                *context = Some(ClinicalContext {
                    cycle: missing(),
                    pregnancy: missing(),
                    menopause: missing(),
                    method: missing(),
                });
            }
            if let Some(c) = context {
                crate::gui::text_field(ui, "Цикл", &mut c.cycle);
                crate::gui::text_field(ui, "Беременность", &mut c.pregnancy);
                crate::gui::text_field(ui, "Менопауза", &mut c.menopause);
                crate::gui::text_field(ui, "Методика", &mut c.method);
            }
        },
    );
    before != serde_json::to_value(context).unwrap()
}
pub fn findings(
    ui: &mut egui::Ui,
    values: &mut Vec<Finding>,
    dictionary: Option<&m53_domain::validation::Dictionary>,
    region: Region,
) {
    for f in &mut *values {
        ui.push_id(f.id, |ui| {
            ui.label(
                dictionary
                    .and_then(|d| d.terms.iter().find(|t| t.code == f.term_code))
                    .map(|t| t.label.as_str())
                    .unwrap_or(&f.term_code),
            );
            egui::ComboBox::from_id_salt("feature-state")
                .selected_text(format!("{:?}", f.observation.value))
                .show_ui(ui, |ui| {
                    for (value, label) in [
                        (m53_domain::FeatureState::Present, "Присутствует"),
                        (m53_domain::FeatureState::Absent, "Отсутствует"),
                        (m53_domain::FeatureState::NotAssessed, "Не оценено"),
                        (m53_domain::FeatureState::NotApplicable, "Не применимо"),
                    ] {
                        ui.selectable_value(&mut f.observation.value, value, label);
                    }
                });
            if matches!(f.observation.origin, Origin::Annotation { .. }) {
                ui.checkbox(&mut f.physician_confirmed, "Подтверждено врачом");
            }
        });
    }
    ui.collapsing("Добавить признак очага", |ui| {
        if let Some(dictionary) = dictionary {
            for term in dictionary.terms.iter().filter(|t| {
                t.region == region && t.kind == m53_domain::validation::TermKind::Finding
            }) {
                if !values.iter().any(|f| f.term_code == term.code)
                    && ui.button(&term.label).clicked()
                {
                    values.push(Finding {
                        id: Uuid::new_v4(),
                        term_code: term.code.clone(),
                        observation: field(m53_domain::FeatureState::NotAssessed),
                        physician_confirmed: true,
                    });
                }
            }
        }
    });
}
pub fn probes(
    ui: &mut egui::Ui,
    tests: &mut Vec<ProcedureTest>,
    dictionary: Option<&m53_domain::validation::Dictionary>,
    sections: &[RegionSection],
) {
    ui.collapsing("Пробы", |ui| {
        if let Some(dictionary) = dictionary {
            for term in dictionary.terms.iter().filter(|t| {
                t.kind == m53_domain::validation::TermKind::Test
                    && sections.iter().any(|s| {
                        s.region == t.region && s.state.value == ExaminationState::Examined
                    })
            }) {
                if !tests
                    .iter()
                    .any(|t| t.code == term.code && t.region == term.region)
                    && ui
                        .button(format!("Добавить {:?}: {}", term.region, term.label))
                        .clicked()
                {
                    tests.push(ProcedureTest {
                        region: term.region,
                        code: term.code.clone(),
                        application: field(TestApplication::NotAssessed),
                        result: missing(),
                    });
                }
            }
        }
        let mut remove = None;
        for (i, test) in tests.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                ui.label(&test.code);
                egui::ComboBox::from_id_salt("test-application")
                    .selected_text(format!("{:?}", test.application.value))
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            (TestApplication::Performed, "Выполнена"),
                            (TestApplication::NotPerformed, "Не выполнена"),
                            (TestApplication::NotAssessed, "Не оценено"),
                            (TestApplication::NotApplicable, "Не применимо"),
                        ] {
                            ui.selectable_value(&mut test.application.value, value, label);
                        }
                    });
                crate::gui::text_field(ui, "Результат пробы", &mut test.result);
                if ui.button("Удалить пробу").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            tests.remove(i);
        }
    });
}
pub fn biopsies(ui: &mut egui::Ui, values: &mut Vec<BiopsySpecimen>, sections: &[RegionSection]) {
    ui.collapsing("Биопсии и материал", |ui| {
        for section in sections
            .iter()
            .filter(|s| s.state.value == ExaminationState::Examined)
        {
            if ui
                .button(format!("Добавить материал: {:?}", section.region))
                .clicked()
            {
                values.push(BiopsySpecimen {
                    id: Uuid::new_v4(),
                    region: section.region,
                    lesion_id: None,
                    location: missing(),
                    material: missing(),
                    sent_for_histology: field(m53_domain::FeatureState::NotAssessed),
                });
            }
        }
        let mut remove = None;
        for (i, b) in values.iter_mut().enumerate() {
            ui.push_id(b.id, |ui| {
                ui.label(format!("Материал {} / {:?}", i + 1, b.region));
                egui::ComboBox::from_id_salt("biopsy-lesion")
                    .selected_text(
                        b.lesion_id
                            .map(|id| format!("Очаг {}", &id.to_string()[..8]))
                            .unwrap_or_else(|| "Без привязки к очагу".into()),
                    )
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut b.lesion_id, None, "Без привязки к очагу");
                        for section in sections.iter().filter(|s| s.region == b.region) {
                            for (i, l) in section.lesions.iter().enumerate() {
                                ui.selectable_value(
                                    &mut b.lesion_id,
                                    Some(l.id),
                                    format!("Очаг {}", i + 1),
                                );
                            }
                        }
                    });
                crate::gui::text_field(ui, "Локализация материала", &mut b.location);
                crate::gui::text_field(ui, "Материал", &mut b.material);
                egui::ComboBox::from_id_salt("histology")
                    .selected_text(format!("{:?}", b.sent_for_histology.value))
                    .show_ui(ui, |ui| {
                        for (value, label) in [
                            (m53_domain::FeatureState::Present, "Направлен"),
                            (m53_domain::FeatureState::Absent, "Не направлен"),
                            (m53_domain::FeatureState::NotAssessed, "Не оценено"),
                            (m53_domain::FeatureState::NotApplicable, "Не применимо"),
                        ] {
                            ui.selectable_value(&mut b.sent_for_histology.value, value, label);
                        }
                    });
                if ui.button("Удалить материал").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            values.remove(i);
        }
    });
}
