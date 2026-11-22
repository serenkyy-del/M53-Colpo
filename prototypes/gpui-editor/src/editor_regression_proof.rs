//! Synthetic pointer/keyboard proofs; never connects to a clinical worker.
use super::*;
use gpui_kit::test::TestWindowExt;

fn reset(view: &Entity<Editor>, cx: &mut App) -> Uuid {
    view.update(cx, |s, cx| {
        let mut photo = Photo::new(1000, 800);
        let mark = photo.place_callout(0, (0.25, 0.55), (0.40, 0.35));
        s.images.clear();
        s.images.insert(
            photo.id,
            Arc::new(RenderImage::new(vec![image::Frame::new(
                image::RgbaImage::from_pixel(1000, 800, image::Rgba([228, 228, 228, 255])),
            )])),
        );
        s.session = Session {
            photos: vec![photo],
            selected: 0,
        };
        s.worker = None;
        s.clinic = None;
        s.properties = None;
        s.ready = true;
        s.readonly = false;
        s.live_active = false;
        s.palette_open = false;
        s.tool = Tool::Select;
        s.fit();
        s.cancel_marker_pick();
        s.before_gesture = None;
        s.undo.clear();
        s.version = 0;
        s.saved = 0;
        s.hover_cursor = CursorStyle::Arrow;
        cx.notify();
        mark
    })
}

fn move_pointer(w: &mut Window, pos: Point<Pixels>, held: bool, cx: &mut App) {
    w.dispatch_event(
        MouseMoveEvent {
            position: pos,
            pressed_button: held.then_some(MouseButton::Left),
            modifiers: Default::default(),
        }
        .to_platform_input(),
        cx,
    );
    w.render_frame(cx);
}

fn down(w: &mut Window, pos: Point<Pixels>, cx: &mut App) {
    w.dispatch_event(
        MouseDownEvent {
            button: MouseButton::Left,
            position: pos,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        }
        .to_platform_input(),
        cx,
    );
    w.render_frame(cx);
}

fn up(w: &mut Window, pos: Point<Pixels>, cx: &mut App) {
    w.dispatch_event(
        MouseUpEvent {
            button: MouseButton::Left,
            position: pos,
            modifiers: Default::default(),
            click_count: 1,
        }
        .to_platform_input(),
        cx,
    );
    w.render_frame(cx);
}

fn key(w: &mut Window, name: &str, cx: &mut App) {
    w.dispatch_event(KeyDownEvent { keystroke: Keystroke::parse(name).unwrap(), is_held: false, prefer_character_input: false }.to_platform_input(), cx);
    w.render_frame(cx);
}

pub(super) fn verify(
    cx: &mut HeadlessAppContext,
    handle: AnyWindowHandle,
    view: &Entity<Editor>,
) -> anyhow::Result<()> {
    let failures = cx.update_window(handle, |_, w, cx| {
        let mut failures = Vec::new();
        let mut check = |ok: bool, message: &str| {
            println!(
                "editor regression {}: {message}",
                if ok { "PASS" } else { "FAIL" }
            );
            if !ok {
                failures.push(message.to_owned());
            }
        };

        let mark = reset(view, cx);
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let pos = point(px(r.x + 0.40 * r.w), px(r.y + 0.35 * r.h));
        move_pointer(w, pos, false, cx);
        down(w, pos, cx);
        up(w, pos, cx);
        check(
            view.read(cx).selected == Some(mark),
            "click selects the real marker",
        );
        check(
            view.read(cx).undo.is_empty() && view.read(cx).version == 0,
            "selection without movement does not consume undo or persist",
        );

        reset(view, cx);
        view.update(cx, |s, cx| {
            let photo=s.photo_mut().unwrap();
            let region=photo.annotation(0.2,0.2,AnnotationLayer::OutputRegion {width:0.3,height:0.3});
            photo.annotations.push(region);
            cx.notify();
        });
        w.render_frame(cx);
        let before_crop=serde_json::json!(view.read(cx).session);
        let r=view.read(cx).image_rect().unwrap();
        w.click("crop",cx);
        let start=point(px(r.x+0.1*r.w),px(r.y+0.1*r.h));
        let far=point(px(r.x+0.8*r.w),px(r.y+0.8*r.h));
        let near=point(px(r.x+0.101*r.w),px(r.y+0.101*r.h));
        down(w,start,cx);move_pointer(w,far,true,cx);move_pointer(w,near,true,cx);up(w,near,cx);
        check(serde_json::json!(view.read(cx).session)==before_crop && view.read(cx).undo.is_empty(),
            "returning crop to origin restores previous region without save or Undo");
        w.click("crop",cx);w.render_frame(cx);
        key(w,"enter",cx);
        for _ in 0..3 {key(w,"right",cx);key(w,"down",cx);}
        key(w,"enter",cx);
        check(serde_json::json!(view.read(cx).session)!=before_crop && view.read(cx).undo.len()==1,
            "keyboard selects output region with two Enter presses and one Undo");
        w.click("undo",cx);
        check(serde_json::json!(view.read(cx).session)==before_crop, "Undo restores pre-keyboard crop");
        w.click("crop",cx);key(w,"enter",cx);
        for _ in 0..3 {key(w,"right",cx);key(w,"down",cx);}
        key(w,"escape",cx);
        check(serde_json::json!(view.read(cx).session)==before_crop, "Escape cancels keyboard crop");
        view.update(cx, |s,cx| {s.readonly=true;w.focus(&s.focus_handle,cx);cx.notify();});
        key(w,"enter",cx);key(w,"right",cx);key(w,"enter",cx);
        check(serde_json::json!(view.read(cx).session)==before_crop, "readonly rejects keyboard crop");
        reset(view, cx);
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let from = point(px(r.x + 0.40 * r.w), px(r.y + 0.35 * r.h));
        let to = point(px(r.x + 0.48 * r.w), px(r.y + 0.42 * r.h));
        let before = serde_json::json!(view.read(cx).session);
        move_pointer(w, from, false, cx);
        down(w, from, cx);
        move_pointer(w, to, true, cx);
        check(
            serde_json::json!(view.read(cx).session) != before,
            "drag updates geometry before release",
        );
        w.press("escape", cx);
        up(w, to, cx);
        check(
            serde_json::json!(view.read(cx).session) == before
                && view.read(cx).gesture.is_none()
                && view.read(cx).undo.is_empty()
                && view.read(cx).version == 0,
            "Esc then release cancels drag without saving",
        );

        reset(view, cx);
        let (tx, rx) = std::sync::mpsc::channel();
        view.update(cx, |s, cx| {
            let mut earlier = s.session.clone();
            earlier.photos[0].annotations.clear();
            s.undo.push(earlier);
            s.readonly = true;
            s.clinic = Some((Uuid::new_v4(), tx));
            cx.notify();
        });
        w.render_frame(cx);
        let before = serde_json::json!(view.read(cx).session);
        w.click("undo", cx);
        check(
            serde_json::json!(view.read(cx).session) == before
                && view.read(cx).version == 0
                && rx.try_recv().is_err(),
            "readonly Undo preserves canvas and sends no worker command",
        );
        view.update(cx, |s, _| s.clinic = None);

        reset(view, cx);
        view.update(cx, |s, cx| {
            s.readonly = true;
            cx.notify();
        });
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let a = &view.read(cx).photo().unwrap().annotations[0];
        let edge = point(
            px(r.x + a.x * r.w + a.radius * r.w.min(r.h)),
            px(r.y + a.y * r.h),
        );
        move_pointer(w, edge, false, cx);
        check(
            !matches!(
                view.read(cx).hover_cursor,
                CursorStyle::ResizeLeftRight
                    | CursorStyle::ResizeUpLeftDownRight
                    | CursorStyle::ResizeUpRightDownLeft
                    | CursorStyle::OpenHand
                    | CursorStyle::ClosedHand
            ),
            "readonly hover does not promise editing",
        );

        let callout = reset(view, cx);
        view.update(cx, |s, cx| {
            let p = s.photo_mut().unwrap();
            let mut target = p.annotation(
                0.50,
                0.50,
                AnnotationLayer::Target {
                    rotation_degrees: 0.,
                    include_in_report: true,
                },
            );
            target.radius = 0.20;
            p.annotations.insert(0, target);
            let a = p.annotations.iter_mut().find(|a| a.id == callout).unwrap();
            a.x = 0.66;
            a.y = 0.50;
            cx.notify();
        });
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let overlap = point(px(r.x + 0.66 * r.w), px(r.y + 0.50 * r.h));
        move_pointer(w, overlap, false, cx);
        let cursor = view.read(cx).hover_cursor;
        down(w, overlap, cx);
        check(
            cursor == CursorStyle::OpenHand && view.read(cx).selected == Some(callout),
            "overlap hover and pointer-down agree on callout body priority",
        );
        up(w, overlap, cx);

        reset(view, cx);
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let from = point(px(r.x + 0.40 * r.w), px(r.y + 0.35 * r.h));
        let to = point(px(r.x + 0.48 * r.w), px(r.y + 0.42 * r.h));
        let before = serde_json::json!(view.read(cx).session);
        w.drag(from, to, cx);
        check(
            view.read(cx).undo.len() == 1 && view.read(cx).version == 1,
            "changed drag creates exactly one undo snapshot and one save",
        );
        let changed = serde_json::json!(view.read(cx).session);
        move_pointer(w, to, false, cx);
        down(w, to, cx);
        up(w, to, cx);
        check(
            view.read(cx).undo.len() == 1 && view.read(cx).version == 1,
            "subsequent selection preserves the real undo entry",
        );
        w.click("undo", cx);
        check(
            serde_json::json!(view.read(cx).session) == before && changed != before,
            "one Undo restores the actual preceding drag after a no-op click",
        );

        reset(view, cx);
        w.render_frame(cx);
        let r = view.read(cx).image_rect().unwrap();
        let from = point(px(r.x + 0.40 * r.w), px(r.y + 0.35 * r.h));
        let to = point(px(r.x + 0.48 * r.w), px(r.y + 0.42 * r.h));
        let before = serde_json::json!(view.read(cx).session);
        move_pointer(w, from, false, cx);
        down(w, from, cx);
        move_pointer(w, to, true, cx);
        view.update(cx, |s, cx| {
            s.readonly = true;
            cx.notify();
        });
        up(w, to, cx);
        check(
            serde_json::json!(view.read(cx).session) == before
                && view.read(cx).undo.is_empty()
                && view.read(cx).version == 0,
            "lock arriving during drag cancels pending geometry on release",
        );

        let (tx, rx) = std::sync::mpsc::channel();
        view.update(cx, |s, cx| {
            let mut earlier = s.session.clone();
            earlier.photos[0].annotations.clear();
            s.undo.push(earlier);
            s.clinic = Some((Uuid::new_v4(), tx));
            // Invoke the production action to cover a queued click after the
            // UI became disabled; it must independently enforce readonly.
            s.undo_action(cx);
        });
        check(
            serde_json::json!(view.read(cx).session) == before
                && view.read(cx).undo.len() == 1
                && view.read(cx).version == 0
                && rx.try_recv().is_err(),
            "readonly action guard rejects an already queued Undo",
        );
        view.update(cx, |s, _| s.clinic = None);

        reset(view, cx);
        let (tx, rx) = std::sync::mpsc::channel();
        view.update(cx, |s, cx| {
            let p = s.photo_mut().unwrap();
            let region = p.annotation(
                0.15,
                0.20,
                AnnotationLayer::OutputRegion {
                    width: 0.60,
                    height: 0.55,
                },
            );
            p.annotations.push(region);
            s.readonly = true;
            s.clinic = Some((Uuid::new_v4(), tx));
            cx.notify();
        });
        w.render_frame(cx);
        let before = serde_json::json!(view.read(cx).session);
        let history = view.read(cx).undo.len();
        let version = view.read(cx).version;
        w.click("whole-photo", cx);
        check(
            serde_json::json!(view.read(cx).session) == before
                && view.read(cx).version == version
                && view.read(cx).undo.len() == history
                && rx.try_recv().is_err(),
            "READONLY-WHOLE-PHOTO: full-image action preserves output region and sends no command",
        );
        view.update(cx, |s, _| s.clinic = None);
        reset(view, cx);
        view.update(cx, |s, cx| {
            s.photo_mut().unwrap().annotations.clear();
            s.choose_preset(0, w, cx);
        });
        w.render_frame(cx);
        key(w, "enter", cx);
        key(w, "shift-right", cx);
        key(w, "enter", cx);
        check(view.read(cx).photo().unwrap().annotations.len()==1 && view.read(cx).undo.len()==1,
            "keyboard places two-point marker with one undo");
        let selected=view.read(cx).selected;
        let before=serde_json::json!(view.read(cx).session);
        key(w, "right", cx);
        check(view.read(cx).selected==selected && serde_json::json!(view.read(cx).session)!=before,
            "focused canvas arrows move selected marker");
        w.click("undo", cx);
        check(serde_json::json!(view.read(cx).session)==before, "undo restores keyboard movement");
        view.update(cx, |s,cx| { s.selected=selected; cx.notify(); });
        let other_focus=cx.focus_handle();
        w.focus(&other_focus,cx);
        key(w, "left", cx);
        check(serde_json::json!(view.read(cx).session)==before, "unfocused canvas ignores keyboard movement");
        view.update(cx, |s,cx| { s.readonly=true; s.selected=selected; w.focus(&s.focus_handle,cx); cx.notify(); });
        key(w, "left", cx);
        check(serde_json::json!(view.read(cx).session)==before, "readonly rejects keyboard movement");
        view.update(cx, |s,cx| { s.readonly=false; s.choose_preset(0,w,cx); });
        key(w, "enter", cx);
        key(w, "escape", cx);
        check(view.read(cx).pending_tip.is_none() && view.read(cx).preset.is_none()
            && serde_json::json!(view.read(cx).session)==before, "Escape cancels keyboard placement without saving");
        failures
    })?;
    anyhow::ensure!(
        failures.is_empty(),
        "editor regressions: {}",
        failures.join("; ")
    );
    Ok(())
}
