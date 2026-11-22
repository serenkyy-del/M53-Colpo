//! Synthetic pointer proof for the real diagnosis form and Kit combobox.
use super::*;
use gpui_kit::test::TestWindowExt;
use m53_client::{diagnoses::Catalog, diagnosis_cache::Snapshot, diagnosis_rows};
use m53_contracts::diagnoses::{Entry, Metadata};
use std::sync::Arc;

pub(super) fn verify(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    view: &Entity<Workplace>,
    directory: &str,
) -> anyhow::Result<()> {
    // Previews open an inactive hidden TestWindow. Real Focus events are emitted
    // only for an active window, even when a FocusHandle already reports focused.
    // Activate this virtual window and drain its platform event before exercising
    // the ordinary textarea → InputEvent::Focus → phrase target subscription.
    cx.update_window(window, |_, w, _| w.activate_window())?;
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        assert!(w.is_window_active(), "template proof requires a genuinely active test window");
        w.render_frame(cx);
    })?;
    let entries = [
        (10, "O06", "Синтетическая группа", true, true),
        (11, "N87", "Синтетическая дисплазия", true, false),
        (12, "N88", "Неактивная синтетическая запись", false, false),
    ]
    .into_iter()
    .map(|(guid, code, name, active, is_folder)| Entry {
        guid: Uuid::from_u128(guid),
        code: code.into(),
        name: name.into(),
        full_name: format!("Полное название: {name}"),
        active,
        is_folder,
        source_version: "synthetic-v1".into(),
    })
    .collect();
    let snapshot = Snapshot {
        catalog: Some(Arc::new(Catalog {
            metadata: Metadata {
                version_id: Uuid::from_u128(100),
                source_id: Uuid::from_u128(101),
                synced_at: "2026-10-26T00:00:00Z".into(),
                count: 3,
            },
            items: entries,
        })),
        priority: Some(Arc::new(m53_client::diagnosis_priority::Group {
            version: 1, catalog_version_id: None, diagnosis_ids: vec![Uuid::from_u128(11)],
        })),
        revision: 1,
        ..Default::default()
    };
    let snapshot_for_settings = snapshot.clone();
    cx.update(|cx| {
        view.update(cx, |s, cx| {
            s.screen = Screen::Protocol;
            s.detail.as_mut().unwrap().study.status = "draft".into();
            s.state["detail"]["study"]["status"] = json!("draft");
            s.diagnoses = snapshot.clone();
            s.form.as_ref().unwrap().update(cx, |f, cx| {
                f.readonly = false;
                f.value["diagnoses"] = json!([]);
                f.value.as_object_mut().unwrap().remove("diagnosis_note");
                f.set_diagnoses(snapshot);
                cx.notify();
            });
            cx.notify();
        })
    });
    let form = cx.update(|cx| view.read(cx).form.as_ref().unwrap().clone());
    for legacy_null in [false, true] {
        cx.update(|cx| form.update(cx, |f, cx| {
            if legacy_null { f.value["diagnosis_note"] = Value::Null; }
            else { f.value.as_object_mut().unwrap().remove("diagnosis_note"); }
            cx.notify();
        }));
        for text in ["", " \n\t "] {
            let before = cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                let before = (form.read(cx).value.clone(), form.read(cx).generation);
                form.read(cx).diagnosis_note_for_test().update(cx, |s, cx| {
                    s.set_value(text, w, cx);
                    cx.emit(InputEvent::Change);
                });
                before
            })?;
            cx.update_window(window, |_, w, cx| {
                w.render_frame(cx);
                assert_eq!(form.read(cx).value, before.0, "initial empty/whitespace input must not materialize an optional note");
                assert_eq!(form.read(cx).generation, before.1, "initial empty/whitespace input is a complete no-op");
            })?;
        }
    }
    let first = cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let scroll = form.read(cx).scroll_for_test();
        scroll.set_offset(point(px(0.), px(-100000.)));
        w.render_frame(cx);
        assert!(
            diagnosis_rows::rows(&form.read(cx).value).is_empty(),
            "blank picker must not select its first item"
        );
        let picker = form.read(cx).diagnosis_picker_for_test(0, cx);
        w.click(("multi-combo-box", picker.entity_id()), cx);
        w.render_frame(cx);
        picker
    })?;
    std::thread::sleep(Duration::from_millis(250));
    cx.update_window(window, |_, w, cx| {
        w.simulate_next_frame(cx);
        w.render_frame(cx);
        let priority = w.find(("diagnosis-option", 1usize)).bounds().origin.y;
        let other = w.find(("diagnosis-option", 0usize)).bounds().origin.y;
        assert!(priority < other, "the preferred diagnosis must be displayed first in the actual picker");
    })?;
    cx.capture_screenshot(window)?.save(std::path::Path::new(directory).join("native-diagnoses-groups.png"))?;
    cx.update_window(window, |_, w, cx| {
        first.update(cx, |s, cx| s.set_query("O06", w, cx));
        w.render_frame(cx);
    })?;
    std::thread::sleep(Duration::from_millis(250));
    cx.update_window(window, |_, w, cx| {
        w.simulate_next_frame(cx);
        w.render_frame(cx);
    })?;
    cx.capture_screenshot(window)?
        .save(std::path::Path::new(directory).join("native-diagnoses-picker.png"))?;
    cx.update_window(window, |_, w, cx| {
        assert_eq!(first.read(cx).query(cx).as_ref(), "O06");
        w.click(("diagnosis-option", 0usize), cx);
    })?;
    let (id, note, short_height) = cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let rows = diagnosis_rows::rows(&form.read(cx).value);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].diagnosis_id,
            Uuid::from_u128(10),
            "active group code must remain selectable"
        );
        let id = rows[0].id;
        let note = form.read(cx).diagnosis_note_for_test();
        let short_height = w.find("diagnosis-note").bounds().size.height;
        note.update(cx, |s, cx| {
            s.set_value("Общее дополнение", w, cx);
            cx.emit(InputEvent::Change);
        });
        w.click("diagnoses-add", cx);
        (id, note, short_height)
    })?;
    let second = cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(
            diagnosis_rows::rows(&form.read(cx).value).len(),
            1,
            "adding a blank row must not add a clinical choice"
        );
        assert_eq!(form.read(cx).value["diagnosis_note"]["value"],
            json!({"state":"assessed","value":"Общее дополнение"}));
        note.focus_handle(cx).focus(w, cx);
        view.update(cx, |s, cx| {
            let mut refreshed = s.diagnoses.catalog.as_ref().unwrap().as_ref().clone();
            refreshed.metadata.version_id = Uuid::from_u128(102);
            s.diagnoses.catalog = Some(Arc::new(refreshed));
            s.diagnoses.revision += 1;
            cx.notify();
        });
        w.render_frame(cx);
        assert_eq!(
            form.read(cx).diagnosis_note_for_test().entity_id(),
            note.entity_id(),
            "catalogue refresh must retain the focused common note entity"
        );
        assert!(
            note.focus_handle(cx).is_focused(w),
            "catalogue refresh must preserve common note keyboard focus"
        );
        let picker = form.read(cx).diagnosis_picker_for_test(1, cx);
        w.click(("multi-combo-box", picker.entity_id()), cx);
        picker.update(cx, |s, cx| s.set_query("дисплазия", w, cx));
        w.render_frame(cx);
        picker
    })?;
    std::thread::sleep(Duration::from_millis(250));
    cx.update_window(window, |_, w, cx| {
        w.simulate_next_frame(cx);
        w.render_frame(cx);
        assert_eq!(second.read(cx).query(cx).as_ref(), "дисплазия");
        view.update(cx, |s, cx| {
            let mut refreshed = s.diagnoses.catalog.as_ref().unwrap().as_ref().clone();
            refreshed.metadata.version_id = Uuid::from_u128(103);
            refreshed.items[1].name = "Синтетическая дисплазия обновлённая".into();
            s.diagnoses.catalog = Some(Arc::new(refreshed));
            s.diagnoses.revision += 1;
            cx.notify();
        });
        w.render_frame(cx);
        assert_eq!(
            form.read(cx).diagnosis_picker_for_test(1, cx).entity_id(),
            second.entity_id(),
            "an open picker must retain the catalogue the doctor is inspecting"
        );
        w.click(("diagnosis-option", 1usize), cx);
    })?;
    let refreshed_picker = cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(
            diagnosis_rows::rows(&form.read(cx).value).len(),
            1,
            "a choice from an obsolete open catalogue must be rejected before clinical mutation"
        );
        let picker = form.read(cx).diagnosis_picker_for_test(1, cx);
        assert_ne!(
            picker.entity_id(),
            second.entity_id(),
            "the closed picker must adopt the refreshed catalogue"
        );
        w.click(("multi-combo-box", picker.entity_id()), cx);
        picker.update(cx, |s, cx| s.set_query("дисплазия", w, cx));
        w.render_frame(cx);
        picker
    })?;
    std::thread::sleep(Duration::from_millis(250));
    cx.update_window(window, |_, w, cx| {
        w.simulate_next_frame(cx);
        w.render_frame(cx);
        assert_eq!(refreshed_picker.read(cx).query(cx).as_ref(), "дисплазия");
        w.click(("diagnosis-option", 1usize), cx);
    })?;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let rows = diagnosis_rows::rows(&form.read(cx).value);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].diagnosis_id, Uuid::from_u128(11));
        assert_eq!(
            rows[1].name, "Синтетическая дисплазия обновлённая",
            "the selected clinical snapshot must match the refreshed visible label"
        );
        w.click(("diagnosis-up", 1usize), cx);
    })?;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let rows = diagnosis_rows::rows(&form.read(cx).value);
        assert_eq!(rows[1].id, id);
        assert!(rows.iter().all(|row| row.additional_text.is_empty()));
        assert_eq!(form.read(cx).value["diagnosis_note"]["value"]["value"], "Общее дополнение");
        form.update(cx, |f, cx| f.focus_phrase_field("/diagnosis_note/value", w, cx).unwrap());
    })?;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(note.focus_handle(cx).is_focused(w), "common note must have real keyboard focus before applying a template");
        let target = form.read(cx).active_phrase_target().expect("common note is a template destination");
        assert_eq!(target.field_code, "diagnosis_note");
        form.update(cx, |f, cx| f.apply_phrase(&target, "Шаблон дополнения\nСтрока 3\nСтрока 4\nСтрока 5\nСтрока 6", false, w, cx).unwrap());
        w.click(("diagnosis-down", 0usize), cx);
    })?;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(diagnosis_rows::rows(&form.read(cx).value)[0].id, id);
        assert_eq!(form.read(cx).value["diagnosis_note"]["value"]["value"],
            "Общее дополнение\nШаблон дополнения\nСтрока 3\nСтрока 4\nСтрока 5\nСтрока 6");
        let note_bounds = w.find("diagnosis-note").bounds();
        assert!(note_bounds.size.height > short_height + px(40.), "common note must grow with content using the conclusion textarea behavior");
        for (index, row) in diagnosis_rows::rows(&form.read(cx).value).into_iter().enumerate() {
            let row_bounds = w.find(SharedString::from(format!("diagnosis-row-{}", row.id))).bounds();
            let picker = form.read(cx).diagnosis_picker_for_test(index, cx);
            let picker_bounds = w.find(("multi-combo-box", picker.entity_id())).bounds();
            assert!(note_bounds.top() >= row_bounds.bottom(), "one common note must appear below all diagnosis rows");
            assert!(row_bounds.size.width - picker_bounds.size.width < px(150.), "diagnosis picker must use the width freed by removing adjacent inputs");
        }
    })?;
    cx.capture_screenshot(window)?
        .save(std::path::Path::new(directory).join("native-diagnoses-ordered.png"))?;
    cx.update_window(window, |_, w, cx| {
        w.click(("diagnosis-remove", 0usize), cx);
    })?;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        note.update(cx, |s, cx| {
            s.set_value("Общее дополнение после удаления строки", w, cx);
            cx.emit(InputEvent::Change);
        });
    })?;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let rows = diagnosis_rows::rows(&form.read(cx).value);
        assert_eq!(rows.len(), 1);
        assert_ne!(rows[0].id, id, "common note editing must not restore a deleted row");
        assert!(rows[0].additional_text.is_empty());
        assert_eq!(form.read(cx).value["diagnosis_note"]["value"]["value"], "Общее дополнение после удаления строки");
        let target = form.read(cx).active_phrase_target().unwrap();
        form.update(cx, |f, cx| {
            f.readonly = true;
            cx.notify();
        });
        w.render_frame(cx);
        let before = form.read(cx).value.clone();
        w.click("diagnoses-add", cx);
        w.click(("diagnosis-remove", 0usize), cx);
        w.click(("diagnosis-up", 0usize), cx);
        note.update(cx, |s, cx| {
            s.set_value("Запоздавшее изменение readonly", w, cx);
            cx.emit(InputEvent::Change);
        });
        assert!(form.update(cx, |f, cx| f.apply_phrase(&target, "Нельзя", false, w, cx)).is_err());
        w.render_frame(cx);
        assert_eq!(
            form.read(cx).value,
            before,
            "readonly pointer controls must not change protocol"
        );
    })?;
    cx.capture_screenshot(window)?
        .save(std::path::Path::new(directory).join("native-diagnoses.png"))?;
    cx.update(|cx| view.update(cx, |s, cx| {
        s.diagnoses = Snapshot::default();
        form.update(cx, |f, cx| {
            f.readonly = false;
            f.value["diagnoses"] = json!([]);
            cx.notify();
        });
        cx.notify();
    }));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find("diagnosis-note").is_some(), "common note remains available without catalogue or selected diagnoses");
        assert_eq!(form.read(cx).diagnosis_note_for_test().entity_id(), note.entity_id());
        note.update(cx, |s, cx| {
            s.set_value("Свободное дополнение без выбранного кода МКБ‑10", w, cx);
            cx.emit(InputEvent::Change);
        });
    })?;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(diagnosis_rows::rows(&form.read(cx).value).is_empty());
        assert_eq!(form.read(cx).value["diagnosis_note"]["value"]["value"], "Свободное дополнение без выбранного кода МКБ‑10");
    })?;
    cx.capture_screenshot(window)?
        .save(std::path::Path::new(directory).join("native-diagnoses-note-only.png"))?;
    let before_clear = cx.update_window(window, |_, w, cx| {
        let generation = form.read(cx).generation;
        note.update(cx, |s, cx| {
            s.set_value("", w, cx);
            cx.emit(InputEvent::Change);
        });
        generation
    })?;
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(form.read(cx).value["diagnosis_note"]["value"], json!({"state":"not_assessed"}), "clearing a present note must persist an explicit empty assessment");
        assert!(form.read(cx).generation > before_clear, "a clear must enter ordinary autosave");
    })?;
    cx.update(|cx| view.update(cx, |s, cx| {
        s.screen = Screen::Settings;
        s.diagnoses = snapshot_for_settings;
        s.priority_draft = Some(m53_client::diagnosis_priority::Draft::new(m53_client::diagnosis_priority::Group {
            version: 1, catalog_version_id: None, diagnosis_ids: vec![Uuid::from_u128(10), Uuid::from_u128(11)],
        }));
        cx.notify();
    }));
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        assert!(w.try_find(("priority-down", 0usize)).is_some(), "priority editor must be present in settings");
        w.click(("priority-down", 0usize), cx);
        w.render_frame(cx);
        assert_eq!(view.read(cx).priority_draft.as_ref().unwrap().ids, vec![Uuid::from_u128(11), Uuid::from_u128(10)]);
        w.click(("priority-remove", 1usize), cx);
        w.render_frame(cx);
        assert_eq!(view.read(cx).priority_draft.as_ref().unwrap().ids, vec![Uuid::from_u128(11)]);
    })?;
    cx.capture_screenshot(window)?
        .save(std::path::Path::new(directory).join("native-diagnosis-priority-settings.png"))?;
    Ok(())
}
