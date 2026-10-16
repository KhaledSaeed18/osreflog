//! osreflog for GNOME (P8a): the history of this system, cause by cause. Read-only: it asks
//! osreflogd over the system bus, which allows reading without a password.

mod backend;
mod view;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use backend::{Failure, Reply, Request};
use gtk::glib;
use osreflog_model::{Cause, CauseDetail, Status};

const APP_ID: &str = "io.github.khaledsaeed18.osreflog";
/// How often the timeline asks for new causes while the window is open.
const REFRESH_S: u32 = 5;

fn main() -> glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build);
    app.run()
}

/// Local time from Unix milliseconds: (day "2026-10-16", time "14:33").
fn local(ms: i64) -> (String, String) {
    glib::DateTime::from_unix_local(ms / 1000)
        .and_then(|d| {
            Ok((
                d.format("%Y-%m-%d")?.to_string(),
                d.format("%H:%M")?.to_string(),
            ))
        })
        .unwrap_or_default()
}

fn local_full(ms: i64) -> String {
    glib::DateTime::from_unix_local(ms / 1000)
        .and_then(|d| d.format("%Y-%m-%d %H:%M:%S").map(|s| s.to_string()))
        .unwrap_or_default()
}

fn day_of(offset_days: i64) -> String {
    let now = glib::DateTime::now_local().ok();
    now.and_then(|d| d.add_days(i32::try_from(offset_days).unwrap_or(0)).ok())
        .and_then(|d| d.format("%Y-%m-%d").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// The widgets the reply loop updates.
struct Ui {
    split: adw::NavigationSplitView,
    title: adw::WindowTitle,
    sidebar: gtk::Stack,
    timeline: gtk::Box,
    down: adw::StatusPage,
    content_page: adw::NavigationPage,
    content: gtk::Stack,
    detail: gtk::Box,
    requests: async_channel::Sender<Request>,
    /// What the timeline shows now, so an unchanged refresh doesn't rebuild it.
    shown: RefCell<Option<(Vec<i64>, String)>>,
}

// The window's whole layout in one place: long, but flat.
#[allow(clippy::too_many_lines)]
fn build(app: &adw::Application) {
    let (requests, replies) = backend::spawn();

    // Sidebar: the timeline, or why there is none.
    let title = adw::WindowTitle::new("History", "");
    let side_header = adw::HeaderBar::new();
    side_header.set_title_widget(Some(&title));
    let timeline = gtk::Box::new(gtk::Orientation::Vertical, 18);
    timeline.set_margin_top(12);
    timeline.set_margin_bottom(12);
    timeline.set_margin_start(12);
    timeline.set_margin_end(12);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&timeline)
        .vexpand(true)
        .build();
    let down = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("osreflog isn't running")
        .description(
            "Start it with  sudo systemctl start osreflogd  — this window picks it up by itself.",
        )
        .build();
    let loading = adw::StatusPage::builder()
        .icon_name("content-loading-symbolic")
        .title("Reading the history…")
        .build();
    let sidebar = gtk::Stack::new();
    sidebar.add_named(&loading, Some("loading"));
    sidebar.add_named(&scroller, Some("timeline"));
    sidebar.add_named(&down, Some("down"));
    sidebar.set_visible_child_name("loading");
    let side_view = adw::ToolbarView::new();
    side_view.add_top_bar(&side_header);
    side_view.set_content(Some(&sidebar));
    let side_page = adw::NavigationPage::builder()
        .title("History")
        .child(&side_view)
        .build();

    // Content: the selected cause.
    let detail = gtk::Box::new(gtk::Orientation::Vertical, 18);
    detail.set_margin_top(12);
    detail.set_margin_bottom(24);
    detail.set_margin_start(12);
    detail.set_margin_end(12);
    let detail_clamp = adw::Clamp::builder()
        .maximum_size(900)
        .child(&detail)
        .build();
    let detail_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&detail_clamp)
        .vexpand(true)
        .build();
    let pick = adw::StatusPage::builder()
        .icon_name("document-open-recent-symbolic")
        .title("Pick a cause")
        .description("Each cause is one thing that changed this system: a command, a service, a package transaction, an app.")
        .build();
    let content = gtk::Stack::new();
    content.add_named(&pick, Some("pick"));
    content.add_named(&detail_scroller, Some("detail"));
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&adw::HeaderBar::new());
    content_view.set_content(Some(&content));
    let content_page = adw::NavigationPage::builder()
        .title("Cause")
        .child(&content_view)
        .build();

    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&side_page));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(340.0);
    split.set_max_sidebar_width(480.0);

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("osreflog")
        .default_width(1100)
        .default_height(760)
        .content(&split)
        .build();
    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        620.0,
        adw::LengthUnit::Sp,
    ));
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    let ui = Rc::new(Ui {
        split,
        title,
        sidebar,
        timeline,
        down,
        content_page,
        content,
        detail,
        requests: requests.clone(),
        shown: RefCell::new(None),
    });

    let ui2 = Rc::clone(&ui);
    glib::spawn_future_local(async move {
        while let Ok(reply) = replies.recv().await {
            match reply {
                Reply::History(r) => show_history(&ui2, r),
                Reply::Detail(id, r) => show_detail(&ui2, id, r),
            }
        }
    });
    let _ = requests.try_send(Request::Refresh);
    let refresh = requests.clone();
    glib::timeout_add_seconds_local(REFRESH_S, move || {
        if refresh.try_send(Request::Refresh).is_err() {
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
    window.present();
}

fn clear(b: &gtk::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

fn show_history(ui: &Rc<Ui>, r: Result<(Status, Vec<Cause>), Failure>) {
    let (status, causes) = match r {
        Ok(v) => v,
        Err(f) => {
            if f.down {
                ui.down.set_title("osreflog isn't running");
                ui.down.set_description(Some(
                    "Start it with  sudo systemctl start osreflogd  — this window picks it up by itself.",
                ));
            } else {
                ui.down.set_title("osreflog didn't answer");
                ui.down.set_description(Some(&f.message));
            }
            ui.title.set_subtitle("");
            ui.sidebar.set_visible_child_name("down");
            *ui.shown.borrow_mut() = None;
            return;
        }
    };
    let line = view::status_line(&status, local_full);
    ui.title.set_subtitle(&line);
    let ids: Vec<i64> = causes.iter().map(|c| c.id).collect();
    let today = day_of(0);
    let key = (ids, today.clone());
    if ui.shown.borrow().as_ref() == Some(&key) {
        ui.sidebar.set_visible_child_name("timeline");
        return;
    }
    clear(&ui.timeline);
    let days = view::days(&causes, local, &today, &day_of(-1));
    if days.is_empty() {
        let empty = adw::StatusPage::builder()
            .icon_name("document-open-recent-symbolic")
            .title("No causes yet")
            .description("osreflog records the commands, services and apps that change this system as they do.")
            .build();
        ui.timeline.append(&empty);
    }
    for day in days {
        let group = adw::PreferencesGroup::builder().title(&day.label).build();
        for row in day.rows {
            let r = adw::ActionRow::builder()
                .title(&row.title)
                .subtitle(&row.subtitle)
                .use_markup(false)
                .activatable(true)
                .build();
            r.add_prefix(&gtk::Image::from_icon_name(row.icon));
            let time = gtk::Label::new(Some(&row.time));
            time.add_css_class("dim-label");
            time.add_css_class("numeric");
            r.add_suffix(&time);
            let ui3 = Rc::clone(ui);
            let id = row.id;
            r.connect_activated(move |_| {
                let _ = ui3.requests.try_send(Request::Show(id));
            });
            group.add(&r);
        }
        ui.timeline.append(&group);
    }
    *ui.shown.borrow_mut() = Some(key);
    ui.sidebar.set_visible_child_name("timeline");
}

fn show_detail(ui: &Rc<Ui>, id: i64, r: Result<CauseDetail, Failure>) {
    clear(&ui.detail);
    ui.content.set_visible_child_name("detail");
    ui.split.set_show_content(true);
    let d = match r {
        Ok(d) => d,
        Err(f) => {
            ui.content_page.set_title(&format!("#{id}"));
            let page = adw::StatusPage::builder()
                .icon_name("dialog-warning-symbolic")
                .title(format!("Cause #{id} couldn't be read"))
                .description(f.message)
                .build();
            ui.detail.append(&page);
            return;
        }
    };
    ui.content_page
        .set_title(&format!("#{} {}", d.cause.id, d.cause.label));

    let heading = gtk::Label::builder()
        .label(&d.cause.label)
        .wrap(true)
        .xalign(0.0)
        .selectable(true)
        .build();
    heading.add_css_class("title-2");
    ui.detail.append(&heading);

    let facts = adw::PreferencesGroup::new();
    for (label, value) in view::facts(&d, local_full) {
        let row = adw::ActionRow::builder()
            .title(label)
            .subtitle(&value)
            .use_markup(false)
            .subtitle_selectable(true)
            .build();
        row.add_css_class("property");
        facts.add(&row);
    }
    ui.detail.append(&facts);

    let changes = adw::PreferencesGroup::builder()
        .title(format!("Changes ({})", d.changes.len()))
        .build();
    for ch in &d.changes {
        let title = view::change_title(ch);
        let subtitle = view::change_subtitle(ch);
        if ch.summary.is_empty() {
            let row = adw::ActionRow::builder()
                .title(&title)
                .subtitle(&subtitle)
                .use_markup(false)
                .build();
            changes.add(&row);
        } else {
            let row = adw::ExpanderRow::builder()
                .title(&title)
                .subtitle(&subtitle)
                .use_markup(false)
                .expanded(d.changes.len() <= 5)
                .build();
            let text = gtk::Label::builder()
                .label(ch.summary.join("\n"))
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .margin_top(8)
                .margin_bottom(8)
                .margin_start(12)
                .margin_end(12)
                .build();
            text.add_css_class("monospace");
            row.add_row(&text);
            changes.add(&row);
        }
    }
    if d.changes.is_empty() {
        let row = adw::ActionRow::builder()
            .title("No file changes recorded for this cause")
            .build();
        changes.add(&row);
    }
    ui.detail.append(&changes);
}
