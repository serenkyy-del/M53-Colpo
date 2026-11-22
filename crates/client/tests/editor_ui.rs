use m53_client::editor_ui::{self, LayerView};
use m53_contracts::workflow::*;
use uuid::Uuid;
fn marker(layer: AnnotationLayer) -> Annotation {
    Annotation {
        id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        lesion_id: None,
        finding_id: None,
        coordinate_system: Default::default(),
        layer,
        x: 0.5,
        y: 0.5,
        radius: 0.05,
        label: "Л".into(),
    }
}
#[test]
fn hiding_screen_layers_survives_restart_without_mutating_report_data() {
    let old = marker(AnnotationLayer::Mark);
    let new = marker(AnnotationLayer::Callout {
        end_x: 0.7,
        end_y: 0.7,
        full_name: "Лейкоплакия".into(),
        term_code: None,
    });
    let target = marker(AnnotationLayer::Target {
        rotation_degrees: 0.0,
        include_in_report: true,
    });
    let annotations = vec![old.clone(), new.clone(), target.clone()];
    let before = serde_json::to_vec(&annotations).unwrap();
    let mut view = LayerView::default();
    assert!(view.visible(&old));
    view.show_legacy = false;
    assert!(!view.visible(&old));
    assert!(view.visible(&new));
    assert!(view.visible(&target));
    view.toggle(new.id);
    assert!(!view.visible(&new));
    let restored: LayerView = serde_json::from_slice(&serde_json::to_vec(&view).unwrap()).unwrap();
    assert!(!restored.visible(&new));
    assert!(!restored.visible(&old));
    assert!(restored.visible(&target));
    assert_eq!(serde_json::to_vec(&annotations).unwrap(), before);
    let default: LayerView = serde_json::from_str("{}").unwrap();
    assert!(default.visible(&old));
}
#[test]
fn native_palette_fits_inspector_and_can_be_selected_with_mouse() {
    let ctx = egui::Context::default();
    editor_ui::configure(&ctx);
    let mut selected = None;
    let input = |events| egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(224.0, 480.0),
        )),
        events,
        ..Default::default()
    };
    let mut render = |events| {
        let _ = ctx.run(input(events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                selected = editor_ui::palette(ui, 0, true).or(selected);
                assert!(
                    ui.min_rect().right() <= 224.0,
                    "palette exceeded inspector width"
                );
            });
        });
    };
    render(vec![]);
    let pos = egui::pos2(35.0, 20.0);
    render(vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        },
    ]);
    render(vec![egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    }]);
    render(vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        },
    ]);
    let drag_pos = egui::pos2(100.0, 130.0);
    render(vec![egui::Event::PointerMoved(drag_pos)]);
    assert_eq!(
        egui::DragAndDrop::payload::<editor_ui::Stamp>(&ctx)
            .unwrap()
            .0,
        0
    );
    render(vec![egui::Event::PointerButton {
        pos: drag_pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    }]);
    assert_eq!(selected, Some(0));
}
