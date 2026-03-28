use baton::{gather_sessions, sway, truncate, SessionInfo};
use glib::timeout_add_seconds_local;
use gtk4::prelude::*;
use gtk4::{
    gdk::Display, style_context_add_provider_for_display, Application, CssProvider, Label,
    Orientation, STYLE_PROVIDER_PRIORITY_USER,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

const CSS: &str = include_str!("../baton-overlay.css");
const AUTO_COLLAPSE_SECS: u32 = 5;

fn main() -> glib::ExitCode {
    let app = Application::builder()
        .application_id("dev.baton.overlay")
        .build();

    // Use connect_activate only once via a flag to prevent duplicate setup
    // on re-activation (e.g. second launch attempt with same app ID).
    let activated = Rc::new(Cell::new(false));
    app.connect_activate(move |app| {
        if activated.get() {
            return;
        }
        activated.set(true);
        build_ui(app);
    });
    app.run()
}

/// Per-monitor widget set.
struct MonitorWidgets {
    session_list: gtk4::Box,
    dots_label: Label,
    footer_label: Label,
    window: gtk4::Window,
}

/// Shared state keyed by monitor connector name — guarantees one overlay per output.
struct SharedState {
    windows: HashMap<String, MonitorWidgets>,
}

/// Get a stable identifier for a monitor. Prefer connector name, fall back to
/// a description-based key so we never silently skip a monitor.
fn monitor_key(monitor: &gtk4::gdk::Monitor) -> String {
    monitor
        .connector()
        .map(|c| c.to_string())
        .unwrap_or_else(|| format!("unknown-{:?}", monitor.geometry()))
}

fn build_ui(app: &Application) {
    let display = Display::default().expect("Could not get default display");
    let monitors = display.monitors();

    load_css();

    let shared = Rc::new(RefCell::new(SharedState {
        windows: HashMap::new(),
    }));

    // Create a window for each existing monitor
    let n = monitors.n_items();
    for i in 0..n {
        let monitor = monitors.item(i).and_downcast::<gtk4::gdk::Monitor>().unwrap();
        ensure_monitor_window(app, &monitor, &shared);
    }

    // Sync windows when monitors change
    let app_weak = app.clone();
    let shared_changed = shared.clone();
    monitors.connect_items_changed(move |list, _position, _removed, _added| {
        sync_monitors(&app_weak, list, &shared_changed);
    });

    // Periodic data refresh — updates all windows at once
    let shared_refresh = shared.clone();
    timeout_add_seconds_local(2, move || {
        let state = shared_refresh.borrow();
        update_all_sessions(&state);
        glib::ControlFlow::Continue
    });

    // Initial data load
    let state = shared.borrow();
    update_all_sessions(&state);
}

/// Reconcile overlay windows with the current set of monitors.
/// Adds windows for new monitors, removes windows for disconnected ones.
fn sync_monitors(
    app: &Application,
    monitor_list: &gtk4::gio::ListModel,
    shared: &Rc<RefCell<SharedState>>,
) {
    let mut current_keys = std::collections::HashSet::new();
    let n = monitor_list.n_items();
    for i in 0..n {
        if let Some(monitor) = monitor_list.item(i).and_downcast::<gtk4::gdk::Monitor>() {
            let key = monitor_key(&monitor);
            current_keys.insert(key);
            ensure_monitor_window(app, &monitor, shared);
        }
    }

    // Remove windows for monitors that no longer exist
    let mut state = shared.borrow_mut();
    let stale: Vec<String> = state
        .windows
        .keys()
        .filter(|k| !current_keys.contains(*k))
        .cloned()
        .collect();
    for key in stale {
        if let Some(widgets) = state.windows.remove(&key) {
            widgets.window.close();
        }
    }
}

/// Create an overlay window for a monitor, but only if one doesn't already exist.
fn ensure_monitor_window(
    app: &Application,
    monitor: &gtk4::gdk::Monitor,
    shared: &Rc<RefCell<SharedState>>,
) {
    let key = monitor_key(monitor);
    if shared.borrow().windows.contains_key(&key) {
        return;
    }
    let outer = gtk4::Box::new(Orientation::Vertical, 0);
    outer.add_css_class("baton-container");

    let inner = gtk4::Box::new(Orientation::Vertical, 0);
    inner.add_css_class("baton-inner");
    outer.append(&inner);

    // Title bar
    let titlebar = gtk4::Box::new(Orientation::Horizontal, 4);
    titlebar.add_css_class("baton-titlebar");

    let dots_label = Label::new(Some(""));
    dots_label.add_css_class("baton-dots");
    titlebar.append(&dots_label);

    inner.append(&titlebar);

    // Expandable body
    let body = gtk4::Box::new(Orientation::Vertical, 0);
    body.add_css_class("baton-body");
    body.set_visible(false);

    let session_list = gtk4::Box::new(Orientation::Vertical, 0);
    session_list.add_css_class("session-list");
    body.append(&session_list);

    let footer = gtk4::Box::new(Orientation::Horizontal, 0);
    footer.add_css_class("baton-footer");
    let footer_label = Label::new(Some(""));
    footer.append(&footer_label);
    body.append(&footer);

    inner.append(&body);

    let window = gtk4::Window::builder()
        .application(app)
        .child(&outer)
        .build();

    window.add_css_class("baton-window");
    window.set_opacity(0.999);

    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("baton"));
    window.set_monitor(Some(monitor));
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Top, 8);
    window.set_margin(Edge::Right, 8);
    window.set_keyboard_mode(KeyboardMode::None);

    // Track hover state
    let is_hovered = Rc::new(Cell::new(false));
    let hover = gtk4::EventControllerMotion::new();
    let hovered_enter = is_hovered.clone();
    hover.connect_enter(move |_, _, _| {
        hovered_enter.set(true);
    });
    let hovered_leave = is_hovered.clone();
    hover.connect_leave(move |_| {
        hovered_leave.set(false);
    });
    outer.add_controller(hover);

    // Auto-collapse timer
    let collapse_counter = Rc::new(Cell::new(0u32));

    let set_expanded = {
        let body = body.clone();
        let window = window.clone();
        Rc::new(move |expanded: bool| {
            body.set_visible(expanded);
            window.set_default_size(if expanded { 520 } else { 1 }, 1);
            window.queue_resize();
        })
    };

    // Click titlebar to toggle expand
    let counter_reset = collapse_counter.clone();
    let set_expanded_click = set_expanded.clone();
    let body_ref = body.clone();
    let click = gtk4::GestureClick::new();
    click.connect_released(move |_, _, _, _| {
        let is_visible = body_ref.is_visible();
        set_expanded_click(!is_visible);
        counter_reset.set(0);
    });
    titlebar.add_controller(click);
    titlebar.set_cursor_from_name(Some("pointer"));

    // Auto-collapse timer (every second)
    let counter_tick = collapse_counter.clone();
    let set_expanded_timer = set_expanded.clone();
    let body_timer = body.clone();
    let hovered_tick = is_hovered.clone();
    glib::timeout_add_seconds_local(1, move || {
        if body_timer.is_visible() {
            if hovered_tick.get() {
                counter_tick.set(0);
            } else {
                let count = counter_tick.get() + 1;
                counter_tick.set(count);
            }
            if counter_tick.get() >= AUTO_COLLAPSE_SECS {
                set_expanded_timer(false);
            }
        }
        glib::ControlFlow::Continue
    });

    // Register in shared state keyed by monitor connector
    {
        let mut state = shared.borrow_mut();
        state.windows.insert(key, MonitorWidgets {
            session_list,
            dots_label,
            footer_label,
            window: window.clone(),
        });
    }

    window.present();
}

fn update_all_sessions(state: &SharedState) {
    let sessions = match gather_sessions() {
        Ok(s) => s,
        Err(_) => {
            for widgets in state.windows.values() {
                widgets.dots_label.set_text("?");
            }
            return;
        }
    };

    let now = chrono::Local::now();
    let timestamp = format!("updated {}", now.format("%H:%M:%S"));

    for widgets in state.windows.values() {
        update_session_widgets(&widgets.session_list, &widgets.dots_label, &widgets.footer_label, &sessions, &timestamp);
    }
}

fn update_session_widgets(session_list: &gtk4::Box, dots_label: &Label, footer_label: &Label, sessions: &[SessionInfo], timestamp: &str) {
    while let Some(child) = session_list.first_child() {
        session_list.remove(&child);
    }

    if sessions.is_empty() {
        dots_label.set_text("");
        let empty = gtk4::Box::new(Orientation::Horizontal, 0);
        empty.add_css_class("baton-empty");
        let label = Label::new(Some("No active Claude Code sessions"));
        empty.append(&label);
        session_list.append(&empty);
    } else {
        dots_label.set_markup(&format_dots_markup(sessions));

        for session in sessions {
            let row = build_session_row(session);
            session_list.append(&row);
        }
    }

    footer_label.set_text(timestamp);
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

    // Top line: task badge + status dot + task description + status label
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

    // Task description (primary) or session name (fallback)
    let display_name = session
        .task_name
        .as_deref()
        .filter(|n| !n.starts_with("Task ")) // skip generic "Task N"
        .unwrap_or(&session.name);
    let name = Label::new(Some(&truncate(display_name, 40)));
    name.add_css_class("session-name");
    name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    top.append(&name);

    // Spacer
    let spacer = gtk4::Box::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    top.append(&spacer);

    // Status label
    let status_text = Label::new(Some(&session.status.label()));
    status_text.add_css_class("status-label");
    status_text.add_css_class(session.status.css_class());
    top.append(&status_text);

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
