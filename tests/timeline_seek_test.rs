use i_slint_backend_testing::ElementQuery;
use memento_app::MementoApp;
use slint::platform::{PointerEventButton, WindowEvent};
use slint::ComponentHandle;
use std::cell::Cell;
use std::rc::Rc;

fn format_time(sec: f32) -> String {
    let total = sec.max(0.0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

#[test]
fn hovering_and_seeking_timeline_does_not_reenter_sharedparley_font_context() {
    i_slint_backend_testing::init_no_event_loop();

    let app = MementoApp::new().unwrap();
    let hover_calls = Rc::new(Cell::new(0usize));
    let hover_calls_cb = hover_calls.clone();
    app.on_format_timestamp(|sec| format_time(sec).into());
    app.on_timeline_hover_moved(move |_| {
        hover_calls_cb.set(hover_calls_cb.get() + 1);
    });
    app.window().set_size(slint::LogicalSize::new(1280.0, 720.0));
    app.set_duration(1442.0);
    app.set_position(13.0);
    app.set_current_time("00:13".into());
    app.set_duration_time("24:02".into());
    app.set_osc_visible(true);
    app.show().unwrap();

    let slider = ElementQuery::from_root(&app)
        .match_id("PlayerOscBar::slider-touch")
        .find_first()
        .expect("PlayerOscBar::slider-touch must exist");
    let pos = slider.absolute_position();
    let size = slider.size();
    assert!(size.width > 50.0 && size.height > 5.0, "unexpected slider size: {size:?}");

    let hover_pos = slint::LogicalPosition::new(pos.x + size.width * 0.4, pos.y + size.height * 0.5);
    app.window().dispatch_event(WindowEvent::PointerMoved {
        position: hover_pos,
    });
    assert!(
        hover_calls.get() > 0,
        "pointer must hit timeline slider at {hover_pos:?} (slider pos={pos:?}, size={size:?})"
    );

    // Click on timeline, update current-time (simulating mpv seek), and evaluate preview card layout
    app.window().dispatch_event(WindowEvent::PointerPressed {
        position: hover_pos,
        button: PointerEventButton::Left,
    });
    app.set_position(765.0);
    app.set_current_time("12:45".into());

    // Re-evaluate all Text sizes (including the timeline hover preview card BEFORE PlayerOscBar layout)
    let _ = ElementQuery::from_root(&app)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .map(|el| el.size())
        .collect::<Vec<_>>();

    let drag_pos = slint::LogicalPosition::new(pos.x + size.width * 0.7, pos.y + size.height * 0.5);
    app.window().dispatch_event(WindowEvent::PointerMoved {
        position: drag_pos,
    });
    app.set_position(3725.0);
    app.set_current_time("01:02:05".into());
    app.set_hover_chapter_title("Chapter 2".into());

    let _ = ElementQuery::from_root(&app)
        .match_type_name("Text")
        .find_all()
        .into_iter()
        .map(|el| el.size())
        .collect::<Vec<_>>();

    app.window().dispatch_event(WindowEvent::PointerReleased {
        position: drag_pos,
        button: PointerEventButton::Left,
    });

    app.hide().unwrap();
}
