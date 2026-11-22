//! Synthetic input proof against the real rendered protocol, without a clinical worker.
use super::*;
use crate::native_form::NativeForm;
use gpui_kit::test::TestWindowExt;

pub(super) fn verify(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    form: &Entity<NativeForm>,
) -> anyhow::Result<()> {
    const TEXT: &str = "Синтетический сохранённый текст";
    cx.update_window(window, |_, w, cx| {
        w.activate_window();
        form.update(cx, |f, cx| {
            f.readonly = true;
            f.value["diagnosis_note"] = json!({
                "kind":"text", "value":{"state":"assessed","value":TEXT}
            });
            cx.notify();
        });
    })?;
    cx.run_until_parked();
    let note = cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        form.read(cx)
            .scroll_for_test()
            .set_offset(point(px(0.), px(-100000.)));
        w.render_frame(cx);
        let note = form.read(cx).diagnosis_note_for_test();
        // Fixture setup uses the public programmatic setter, never as an edit attempt.
        note.update(cx, |s, cx| s.set_value(TEXT, w, cx));
        w.render_frame(cx);
        note
    })?;
    cx.run_until_parked();
    cx.update_window(window, |_, w, cx| {
        w.render_frame(cx);
        let before = (form.read(cx).value.clone(), form.read(cx).generation);
        let bounds = w.find("diagnosis-note").bounds();
        w.click_at("diagnosis-note", point(px(30.), bounds.size.height - px(12.)), cx);
        w.render_frame(cx);
        anyhow::ensure!(note.focus_handle(cx).is_focused(w),
            "readonly saved text must accept ordinary pointer focus for reading and copying");
        w.press("ctrl-a", cx);
        anyhow::ensure!(note.read(cx).selected_range() == (0..TEXT.len()),
            "readonly saved text must support selecting its complete contents");
        w.press("ctrl-c", cx);
        let copied = cx.read_from_clipboard().and_then(|item| item.text());
        anyhow::ensure!(copied.as_deref() == Some(TEXT),
            "readonly saved text must be copyable through the native input handler");
        w.input("Попытка изменения", cx);
        w.press("backspace", cx);
        w.press("delete", cx);
        cx.write_to_clipboard(ClipboardItem::new_string("Вставка".into()));
        w.press("ctrl-v", cx);
        w.render_frame(cx);
        anyhow::ensure!(note.read(cx).value().as_ref() == TEXT,
            "typing, deletion and paste must preserve readonly input contents");
        anyhow::ensure!(form.read(cx).value == before.0 && form.read(cx).generation == before.1,
            "readonly input must not mutate protocol values or generation");
        let presentation = note.read(cx).presentation();
        anyhow::ensure!(presentation.is_readonly() && !presentation.is_disabled(),
            "saved clinical text must use native readonly presentation, without disabled dimming");
        println!("readonly protocol PASS: pointer focus, selection/copy, blocked typing/delete/paste, unchanged model");
        Ok(())
    })?
}
