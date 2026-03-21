use baton::{gather_sessions, shorten_path, sway, truncate, SessionInfo};
use glib::timeout_add_seconds_local;
use gtk4::prelude::*;
use gtk4::{
    gdk::Display, style_context_add_provider_for_display, Application, CssProvider, Label,
    Orientation, STYLE_PROVIDER_PRIORITY_USER,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::RefCell;
use std::rc::Rc;

const CSS: &str = include_str!("../baton-overlay.css");

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
    let titlebar = gtk4::Box::new(Orientation::Horizontal, 6);
    titlebar.add_css_class("baton-titlebar");

    let title_label = Label::new(Some("BATON"));
    titlebar.append(&title_label);

    let count_label = Label::new(Some(""));
    count_label.add_css_class("baton-title-count");
    titlebar.append(&count_label);

    let spacer = gtk4::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    titlebar.append(&spacer);

    // Collapse/expand toggle
    let toggle_btn = gtk4::Button::with_label("▾");
    toggle_btn.add_css_class("baton-toggle");
    titlebar.append(&toggle_btn);

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

    // Collapsible body
    let body = gtk4::Box::new(Orientation::Vertical, 0);
    body.add_css_class("baton-body");

    let session_list = gtk4::Box::new(Orientation::Vertical, 0);
    session_list.add_css_class("session-list");
    body.append(&session_list);

    let footer = gtk4::Box::new(Orientation::Horizontal, 0);
    footer.add_css_class("baton-footer");
    let footer_label = Label::new(Some(""));
    footer.append(&footer_label);
    body.append(&footer);

    inner.append(&body);

    // Collapse toggle
    let body_clone = body.clone();
    let toggle_btn_clone = toggle_btn.clone();
    toggle_btn.connect_clicked(move |_| {
        let visible = body_clone.is_visible();
        body_clone.set_visible(!visible);
        toggle_btn_clone.set_label(if visible { "▸" } else { "▾" });
    });

    let window = gtk4::Window::builder()
        .application(app)
        .child(&outer)
        .build();

    // Make window background fully transparent so rounded corners show through
    window.add_css_class("baton-window");

    // Near-unity opacity forces compositor alpha blending so the
    // transparent window background composites correctly (Sway #8904).
    window.set_opacity(0.999);

    // Layer shell — sticky on all workspaces by default
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("baton"));
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Top, 8);
    window.set_margin(Edge::Right, 8);
    window.set_keyboard_mode(KeyboardMode::None);

    load_css();
    update_sessions(&session_list, &count_label, &footer_label);

    let session_list = Rc::new(RefCell::new(session_list));
    let count_label = Rc::new(RefCell::new(count_label));
    let footer_label = Rc::new(RefCell::new(footer_label));

    timeout_add_seconds_local(2, move || {
        update_sessions(
            &session_list.borrow(),
            &count_label.borrow(),
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

fn update_sessions(session_list: &gtk4::Box, count_label: &Label, footer_label: &Label) {
    while let Some(child) = session_list.first_child() {
        session_list.remove(&child);
    }

    let sessions = match gather_sessions() {
        Ok(s) => s,
        Err(_) => {
            let empty = gtk4::Box::new(Orientation::Horizontal, 0);
            empty.add_css_class("baton-empty");
            let label = Label::new(Some("Error scanning sessions"));
            empty.append(&label);
            session_list.append(&empty);
            return;
        }
    };

    if sessions.is_empty() {
        count_label.set_text("0");
        let empty = gtk4::Box::new(Orientation::Horizontal, 0);
        empty.add_css_class("baton-empty");
        let label = Label::new(Some("No active Claude Code sessions"));
        empty.append(&label);
        session_list.append(&empty);
    } else {
        count_label.set_text(&format!("{}", sessions.len()));
        for session in &sessions {
            let row = build_session_row(session);
            session_list.append(&row);
        }
    }

    let now = chrono::Local::now();
    footer_label.set_text(&format!("updated {}", now.format("%H:%M:%S")));
}

fn build_session_row(session: &SessionInfo) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Vertical, 2);
    row.add_css_class("session-row");
    row.add_css_class(session.status.css_class());

    if let Some(t) = session.task_num {
        row.add_css_class(&format!("task{t}"));
    }

    // Top line: task badge + name + status
    let top = gtk4::Box::new(Orientation::Horizontal, 6);

    // Task badge
    let task_text = session
        .task_num
        .map(|t| format!("T{t}"))
        .unwrap_or_else(|| "??".to_string());
    let task_label = Label::new(Some(&task_text));
    task_label.add_css_class("session-task");
    top.append(&task_label);

    // Status dot
    let dot = Label::new(Some(session.status.dot()));
    dot.add_css_class("status-dot");
    dot.add_css_class(session.status.css_class());
    top.append(&dot);

    // Name (session/branch)
    let name = Label::new(Some(&truncate(&session.name, 20)));
    name.add_css_class("session-name");
    name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    top.append(&name);

    // Status label
    let status_text = Label::new(Some(&session.status.label()));
    status_text.add_css_class("status-label");
    status_text.add_css_class(session.status.css_class());
    top.append(&status_text);

    // Spacer
    let spacer = gtk4::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top.append(&spacer);

    // PWD (small, right-aligned)
    let pwd = Label::new(Some(&shorten_path(&session.cwd, 24)));
    pwd.add_css_class("session-pwd");
    pwd.set_ellipsize(gtk4::pango::EllipsizeMode::Start);
    top.append(&pwd);

    row.append(&top);

    // Bottom line: what it's doing (the main info)
    if !session.doing.is_empty() {
        let doing = Label::new(Some(&truncate(&session.doing, 70)));
        doing.add_css_class("session-doing");
        doing.set_xalign(0.0);
        doing.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        doing.set_margin_start(32); // indent past task badge
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
