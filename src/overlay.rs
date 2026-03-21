use baton::{gather_sessions, shorten_path, sway, truncate, SessionInfo};
use glib::timeout_add_seconds_local;
use gtk4::prelude::*;
use gtk4::{
    gdk::Display, style_context_add_provider_for_display, Application, CssProvider, Label,
    Orientation, STYLE_PROVIDER_PRIORITY_USER,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const CSS: &str = include_str!("../baton-overlay.css");
const AUTO_COLLAPSE_SECS: u32 = 5;

fn main() -> glib::ExitCode {
    let app = Application::builder()
        .application_id("dev.baton.overlay")
        .build();

    app.connect_activate(build_ui);
    app.run()
}

fn build_ui(app: &Application) {
    let outer = gtk4::Box::new(Orientation::Vertical, 0);
    outer.add_css_class("baton-container");

    let inner = gtk4::Box::new(Orientation::Vertical, 0);
    inner.add_css_class("baton-inner");
    outer.append(&inner);

    // Title bar
    let titlebar = gtk4::Box::new(Orientation::Horizontal, 4);
    titlebar.add_css_class("baton-titlebar");

    let title_label = Label::new(Some("BATON"));
    titlebar.append(&title_label);

    // Compact dots (visible when collapsed)
    let dots_label = Label::new(Some(""));
    dots_label.add_css_class("baton-dots");
    titlebar.append(&dots_label);

    let spacer = gtk4::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    titlebar.append(&spacer);

    // Close button
    let close_btn = gtk4::Button::with_label("×");
    close_btn.add_css_class("baton-close");
    close_btn.connect_clicked(|btn| {
        if let Some(w) = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok()) {
            w.close();
        }
    });
    titlebar.append(&close_btn);

    inner.append(&titlebar);

    // Expandable body
    let body = gtk4::Box::new(Orientation::Vertical, 0);
    body.add_css_class("baton-body");
    body.set_visible(false); // start collapsed

    let session_list = gtk4::Box::new(Orientation::Vertical, 0);
    session_list.add_css_class("session-list");
    body.append(&session_list);

    let footer = gtk4::Box::new(Orientation::Horizontal, 0);
    footer.add_css_class("baton-footer");
    let footer_label = Label::new(Some(""));
    footer.append(&footer_label);
    body.append(&footer);

    inner.append(&body);

    // Auto-collapse timer: incremented each second, resets on click
    let collapse_counter = Rc::new(Cell::new(0u32));

    // Click titlebar to expand, auto-collapse after N seconds
    let body_expand = body.clone();
    let counter_reset = collapse_counter.clone();
    let click = gtk4::GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        let is_visible = body_expand.is_visible();
        body_expand.set_visible(!is_visible);
        counter_reset.set(0);
    });
    titlebar.add_controller(click);
    titlebar.set_cursor_from_name(Some("pointer"));

    // Auto-collapse timer (every second)
    let body_collapse = body.clone();
    let counter_tick = collapse_counter.clone();
    glib::timeout_add_seconds_local(1, move || {
        if body_collapse.is_visible() {
            let count = counter_tick.get() + 1;
            counter_tick.set(count);
            if count >= AUTO_COLLAPSE_SECS {
                body_collapse.set_visible(false);
            }
        }
        glib::ControlFlow::Continue
    });

    let window = gtk4::Window::builder()
        .application(app)
        .child(&outer)
        .build();

    window.add_css_class("baton-window");
    window.set_opacity(0.999);

    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("baton"));
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Top, 8);
    window.set_margin(Edge::Right, 8);
    window.set_keyboard_mode(KeyboardMode::None);

    load_css();
    update_sessions(&session_list, &dots_label, &footer_label);

    let session_list = Rc::new(RefCell::new(session_list));
    let dots_label = Rc::new(RefCell::new(dots_label));
    let footer_label = Rc::new(RefCell::new(footer_label));

    timeout_add_seconds_local(2, move || {
        update_sessions(
            &session_list.borrow(),
            &dots_label.borrow(),
            &footer_label.borrow(),
        );
        glib::ControlFlow::Continue
    });

    window.present();
}

fn load_css() {
    let provider = CssProvider::new();
    provider.load_from_string(CSS);

    style_context_add_provider_for_display(
        &Display::default().expect("Could not get default display"),
        &provider,
        STYLE_PROVIDER_PRIORITY_USER,
    );
}

fn update_sessions(session_list: &gtk4::Box, dots_label: &Label, footer_label: &Label) {
    while let Some(child) = session_list.first_child() {
        session_list.remove(&child);
    }

    let sessions = match gather_sessions() {
        Ok(s) => s,
        Err(_) => {
            dots_label.set_text("?");
            return;
        }
    };

    if sessions.is_empty() {
        dots_label.set_text("");
        let empty = gtk4::Box::new(Orientation::Horizontal, 0);
        empty.add_css_class("baton-empty");
        let label = Label::new(Some("No active Claude Code sessions"));
        empty.append(&label);
        session_list.append(&empty);
    } else {
        // Build compact dots string for collapsed view: "● ● ○"
        let dots: String = sessions
            .iter()
            .map(|s| s.status.dot())
            .collect::<Vec<_>>()
            .join(" ");
        dots_label.set_markup(&format_dots_markup(&sessions));
        let _ = dots; // used via format_dots_markup

        for session in &sessions {
            let row = build_session_row(session);
            session_list.append(&row);
        }
    }

    let now = chrono::Local::now();
    footer_label.set_text(&format!("updated {}", now.format("%H:%M:%S")));
}

/// Build colored dots markup for the title bar
fn format_dots_markup(sessions: &[SessionInfo]) -> String {
    let mut parts = Vec::new();
    for s in sessions {
        let (color, dot) = match &s.status {
            baton::SessionStatus::Working => ("#3ddc84", "●"),
            baton::SessionStatus::Idle(_) => ("#f0b429", "○"),
            baton::SessionStatus::Stuck => ("#ff5f5f", "⚠"),
            baton::SessionStatus::Stopped => ("#6060a0", "◌"),
        };
        parts.push(format!("<span foreground=\"{color}\">{dot}</span>"));
    }
    parts.join(" ")
}

fn build_session_row(session: &SessionInfo) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Vertical, 2);
    row.add_css_class("session-row");
    row.add_css_class(session.status.css_class());

    if let Some(t) = session.task_num {
        row.add_css_class(&format!("task{t}"));
    }

    // Top line: task badge + status dot + name + status label + pwd
    let top = gtk4::Box::new(Orientation::Horizontal, 6);

    let task_text = session
        .task_num
        .map(|t| format!("T{t}"))
        .unwrap_or_else(|| "??".to_string());
    let task_label = Label::new(Some(&task_text));
    task_label.add_css_class("session-task");
    top.append(&task_label);

    let dot = Label::new(Some(session.status.dot()));
    dot.add_css_class("status-dot");
    dot.add_css_class(session.status.css_class());
    top.append(&dot);

    let name = Label::new(Some(&truncate(&session.name, 20)));
    name.add_css_class("session-name");
    name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    top.append(&name);

    let status_text = Label::new(Some(&session.status.label()));
    status_text.add_css_class("status-label");
    status_text.add_css_class(session.status.css_class());
    top.append(&status_text);

    let spacer = gtk4::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top.append(&spacer);

    let pwd = Label::new(Some(&shorten_path(&session.cwd, 24)));
    pwd.add_css_class("session-pwd");
    pwd.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
    top.append(&pwd);

    row.append(&top);

    // Bottom line: what it's doing
    if !session.doing.is_empty() {
        let doing = Label::new(Some(&truncate(&session.doing, 70)));
        doing.add_css_class("session-doing");
        doing.set_xalign(0.0);
        doing.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        doing.set_margin_start(32);
        row.append(&doing);
    }

    // Click to switch workspace
    if let Some(ws) = &session.workspace {
        let ws = ws.clone();
        let click = gtk4::GestureClick::new();
        click.connect_released(move |_, _, _, _| {
            let _ = sway::switch_to_workspace(&ws);
        });
        row.add_controller(click);
        row.set_cursor_from_name(Some("pointer"));
    }

    row
}
