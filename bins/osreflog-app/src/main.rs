//! osreflog for GNOME: the history of this system, cause by cause (P8a), and what changed
//! between two checkpoints (P8b). Read-only: it asks osreflogd over the system bus, which allows
//! reading without a password.

mod backend;
mod view;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use backend::{Failure, History, Reply, Request};
use gtk::glib;
use osreflog_model::{CauseDetail, DiffReport, LogEntry};

const APP_ID: &str = "io.github.khaledsaeed18.osreflog";
/// How often the lists ask for news while the window is open.
const REFRESH_S: u32 = 5;
/// At most this many files per group in a diff; the rest are counted.
const MAX_ROWS: usize = 400;
/// The live system, as a diff's newer side.
const NOW: &str = "@{now}";

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

/// What the content side shows, or is waiting for; a reply for anything else is dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selected {
    Cause(i64),
    Diff { from: String, to: String, all: bool },
}

/// What a list shows now (its keys and the day it was built on), so an unchanged refresh
/// doesn't rebuild it.
type Shown<T> = RefCell<Option<(Vec<T>, String)>>;

/// The widgets the reply loop updates.
struct Ui {
    split: adw::NavigationSplitView,
    title: adw::WindowTitle,
    sidebar: gtk::Stack,
    timeline: gtk::Box,
    checkpoints: gtk::Box,
    down: adw::StatusPage,
    content_page: adw::NavigationPage,
    content: gtk::Stack,
    busy: adw::StatusPage,
    detail: gtk::Box,
    requests: async_channel::Sender<Request>,
    selected: RefCell<Option<Selected>>,
    /// The newest checkpoint names (newest first), for the diff's "until" choice.
    names: RefCell<Vec<String>>,
    /// What the two lists show now, so an unchanged refresh doesn't rebuild them.
    shown_causes: Shown<i64>,
    shown_checkpoints: Shown<(String, bool)>,
}

/// A row showing text from the system as it is. Setters in this order on purpose: with the
/// builder, `use-markup` arrived after the text whatever the call order, and every `&` in a
/// command (`a && b`) was parsed as markup first.
fn action_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_use_markup(false);
    row.set_title(title);
    if !subtitle.is_empty() {
        row.set_subtitle(subtitle);
    }
    row
}

fn page_box() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 18);
    b.set_margin_top(12);
    b.set_margin_bottom(12);
    b.set_margin_start(12);
    b.set_margin_end(12);
    b
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .vexpand(true)
        .build()
}

// The window's whole layout in one place: long, but flat.
#[allow(clippy::too_many_lines)]
fn build(app: &adw::Application) {
    let (requests, replies) = backend::spawn();

    // Sidebar: causes or checkpoints, or why there are none.
    let title = adw::WindowTitle::new("History", "");
    let side_header = adw::HeaderBar::new();
    side_header.set_title_widget(Some(&title));
    let timeline = page_box();
    let checkpoints = page_box();
    let views = adw::ViewStack::new();
    views.add_titled_with_icon(
        &scrolled(&timeline),
        Some("causes"),
        "Causes",
        "document-open-recent-symbolic",
    );
    views.add_titled_with_icon(
        &scrolled(&checkpoints),
        Some("checkpoints"),
        "Checkpoints",
        "bookmark-new-symbolic",
    );
    let switcher = adw::ViewSwitcher::builder()
        .stack(&views)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let switcher_bar = gtk::CenterBox::new();
    switcher_bar.set_center_widget(Some(&switcher));
    switcher_bar.set_margin_bottom(6);
    let down = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("osreflog isn't running")
        .build();
    let loading = adw::StatusPage::builder()
        .icon_name("content-loading-symbolic")
        .title("Reading the history…")
        .build();
    let sidebar = gtk::Stack::new();
    sidebar.add_named(&loading, Some("loading"));
    sidebar.add_named(&views, Some("views"));
    sidebar.add_named(&down, Some("down"));
    sidebar.set_visible_child_name("loading");
    let side_view = adw::ToolbarView::new();
    side_view.add_top_bar(&side_header);
    side_view.add_top_bar(&switcher_bar);
    side_view.set_content(Some(&sidebar));
    let side_page = adw::NavigationPage::builder()
        .title("History")
        .child(&side_view)
        .build();

    // Content: the selected cause or diff.
    let detail = page_box();
    detail.set_margin_bottom(24);
    let detail_clamp = adw::Clamp::builder()
        .maximum_size(900)
        .child(&detail)
        .build();
    let pick = adw::StatusPage::builder()
        .icon_name("document-open-recent-symbolic")
        .title("Pick a cause or a checkpoint")
        .description("A cause is one thing that changed this system: a command, a service, a package transaction, an app. A checkpoint shows what changed since it.")
        .build();
    let busy = adw::StatusPage::builder()
        .icon_name("content-loading-symbolic")
        .build();
    let content = gtk::Stack::new();
    content.add_named(&pick, Some("pick"));
    content.add_named(&busy, Some("busy"));
    content.add_named(&scrolled(&detail_clamp), Some("detail"));
    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&adw::HeaderBar::new());
    content_view.set_content(Some(&content));
    let content_page = adw::NavigationPage::builder()
        .title("osreflog")
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
        checkpoints,
        down,
        content_page,
        content,
        busy,
        detail,
        requests: requests.clone(),
        selected: RefCell::new(None),
        names: RefCell::new(Vec::new()),
        shown_causes: RefCell::new(None),
        shown_checkpoints: RefCell::new(None),
    });

    let ui2 = Rc::clone(&ui);
    glib::spawn_future_local(async move {
        while let Ok(reply) = replies.recv().await {
            match reply {
                Reply::History(r) => show_history(&ui2, r),
                Reply::Detail(id, r) => {
                    if *ui2.selected.borrow() == Some(Selected::Cause(id)) {
                        show_detail(&ui2, id, r);
                    }
                }
                Reply::Diff {
                    from,
                    to,
                    all,
                    result,
                } => {
                    let this = Selected::Diff {
                        from: from.clone(),
                        to: to.clone(),
                        all,
                    };
                    if *ui2.selected.borrow() == Some(this) {
                        show_diff(&ui2, &from, &to, all, result);
                    }
                }
            }
        }
    });
    let _ = requests.try_send(Request::Refresh);
    // Development: open a page at start, `since:c000134` or `cause:765` (input through GTK's
    // Broadway backend, used to look at the window from a browser, drops clicks).
    if let Ok(open) = std::env::var("OSREFLOG_APP_OPEN") {
        match open.split_once(':') {
            Some(("since", name)) => select_diff(&ui, name, NOW, false),
            Some(("cause", id)) => {
                if let Ok(id) = id.parse() {
                    select_cause(&ui, id);
                }
            }
            _ => eprintln!("osreflog-app: OSREFLOG_APP_OPEN is since:<checkpoint> or cause:<id>"),
        }
    }
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

/// Asks for a cause, showing that it's coming.
fn select_cause(ui: &Rc<Ui>, id: i64) {
    *ui.selected.borrow_mut() = Some(Selected::Cause(id));
    let _ = ui.requests.try_send(Request::Show(id));
}

/// Asks for a diff, showing that it's being computed (a diff to now can take seconds).
fn select_diff(ui: &Rc<Ui>, from: &str, to: &str, all: bool) {
    *ui.selected.borrow_mut() = Some(Selected::Diff {
        from: from.to_owned(),
        to: to.to_owned(),
        all,
    });
    let until = if to == NOW { "now" } else { to };
    ui.busy
        .set_title(&format!("Comparing {from} with {until}…"));
    ui.busy.set_description(Some(
        "osreflog reads the difference between the two snapshots.",
    ));
    ui.content.set_visible_child_name("busy");
    ui.split.set_show_content(true);
    let _ = ui.requests.try_send(Request::Diff {
        from: from.to_owned(),
        to: to.to_owned(),
        all,
    });
}

fn show_history(ui: &Rc<Ui>, r: Result<History, Failure>) {
    let (status, causes, log) = match r {
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
            *ui.shown_causes.borrow_mut() = None;
            *ui.shown_checkpoints.borrow_mut() = None;
            return;
        }
    };
    ui.title
        .set_subtitle(&view::status_line(&status, local_full));
    let today = day_of(0);
    show_causes(ui, &causes, &today);
    show_checkpoints(ui, &log, &today);
    ui.sidebar.set_visible_child_name("views");
}

fn time_suffix(text: &str) -> gtk::Label {
    let time = gtk::Label::new(Some(text));
    time.add_css_class("dim-label");
    time.add_css_class("numeric");
    time
}

fn show_causes(ui: &Rc<Ui>, causes: &[osreflog_model::Cause], today: &str) {
    let key = (causes.iter().map(|c| c.id).collect(), today.to_owned());
    if ui.shown_causes.borrow().as_ref() == Some(&key) {
        return;
    }
    clear(&ui.timeline);
    let days = view::days(causes, local, today, &day_of(-1));
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
            let r = action_row(&row.title, &row.subtitle);
            r.set_activatable(true);
            r.add_prefix(&gtk::Image::from_icon_name(row.icon));
            r.add_suffix(&time_suffix(&row.time));
            let ui3 = Rc::clone(ui);
            let id = row.id;
            r.connect_activated(move |_| select_cause(&ui3, id));
            group.add(&r);
        }
        ui.timeline.append(&group);
    }
    *ui.shown_causes.borrow_mut() = Some(key);
}

fn show_checkpoints(ui: &Rc<Ui>, log: &[LogEntry], today: &str) {
    let key = (
        log.iter()
            .map(|e| (e.checkpoint.name.clone(), e.interval.is_some()))
            .collect(),
        today.to_owned(),
    );
    if ui.shown_checkpoints.borrow().as_ref() == Some(&key) {
        return;
    }
    *ui.names.borrow_mut() = log.iter().map(|e| e.checkpoint.name.clone()).collect();
    clear(&ui.checkpoints);
    for (label, rows) in view::checkpoint_days(log, local, today, &day_of(-1)) {
        let group = adw::PreferencesGroup::builder().title(&label).build();
        for row in rows {
            let r = action_row(&row.title, &row.subtitle);
            r.set_activatable(true);
            r.add_prefix(&gtk::Image::from_icon_name(row.icon));
            r.add_suffix(&time_suffix(&row.time));
            let ui3 = Rc::clone(ui);
            let name = row.name.clone();
            r.connect_activated(move |_| select_diff(&ui3, &name, NOW, false));
            group.add(&r);
        }
        ui.checkpoints.append(&group);
    }
    *ui.shown_checkpoints.borrow_mut() = Some(key);
}

fn failure_page(ui: &Ui, title: &str, f: Failure) {
    let page = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title(title)
        .description(f.message)
        .build();
    ui.detail.append(&page);
    ui.content.set_visible_child_name("detail");
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .selectable(true)
        .build();
    l.add_css_class("title-2");
    l
}

fn facts_group(facts: Vec<(&'static str, String)>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    for (label, value) in facts {
        let row = action_row(label, &value);
        row.set_subtitle_selectable(true);
        row.add_css_class("property");
        group.add(&row);
    }
    group
}

/// A file row: an expander with its summary lines when it has any.
fn file_row(title: &str, subtitle: &str, summary: &[String], expanded: bool) -> gtk::Widget {
    if summary.is_empty() {
        return action_row(title, subtitle).upcast();
    }
    let row = adw::ExpanderRow::new();
    row.set_use_markup(false);
    row.set_title(title);
    row.set_subtitle(subtitle);
    row.set_expanded(expanded);
    let text = gtk::Label::builder()
        .label(summary.join("\n"))
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
    row.upcast()
}

fn show_detail(ui: &Rc<Ui>, id: i64, r: Result<CauseDetail, Failure>) {
    clear(&ui.detail);
    ui.split.set_show_content(true);
    let d = match r {
        Ok(d) => d,
        Err(f) => {
            ui.content_page.set_title(&format!("#{id}"));
            failure_page(ui, &format!("Cause #{id} couldn't be read"), f);
            return;
        }
    };
    ui.content_page
        .set_title(&format!("#{} {}", d.cause.id, d.cause.label));
    ui.detail.append(&heading(&d.cause.label));
    ui.detail.append(&facts_group(view::facts(&d, local_full)));

    let changes = adw::PreferencesGroup::builder()
        .title(format!("Changes ({})", d.changes.len()))
        .build();
    for ch in &d.changes {
        changes.add(&file_row(
            &view::change_title(ch),
            &view::change_subtitle(ch),
            &ch.summary,
            d.changes.len() <= 5,
        ));
    }
    if d.changes.is_empty() {
        changes.add(
            &adw::ActionRow::builder()
                .title("No file changes recorded for this cause")
                .build(),
        );
    }
    ui.detail.append(&changes);
    ui.content.set_visible_child_name("detail");
}

/// The "until" choice: now, or any checkpoint newer than `from`.
fn until_row(ui: &Rc<Ui>, from: &str, to: &str, all: bool) -> adw::ComboRow {
    let names = ui.names.borrow();
    let newer: Vec<String> = names
        .iter()
        .take_while(|n| n.as_str() != from)
        .cloned()
        .collect();
    let mut choices = vec!["now".to_owned()];
    choices.extend(newer);
    let strings: Vec<&str> = choices.iter().map(String::as_str).collect();
    let model = gtk::StringList::new(&strings);
    let row = adw::ComboRow::builder()
        .title("Until")
        .model(&model)
        .build();
    let current = if to == NOW { "now" } else { to };
    if let Some(i) = choices.iter().position(|c| c == current) {
        row.set_selected(u32::try_from(i).unwrap_or(0));
    }
    let ui3 = Rc::clone(ui);
    let from = from.to_owned();
    row.connect_selected_notify(move |r| {
        let i = usize::try_from(r.selected()).unwrap_or(0);
        let Some(choice) = choices.get(i) else { return };
        let to = if choice == "now" {
            NOW
        } else {
            choice.as_str()
        };
        select_diff(&ui3, &from, to, all);
    });
    row
}

fn show_diff(ui: &Rc<Ui>, from: &str, to: &str, all: bool, r: Result<Box<DiffReport>, Failure>) {
    clear(&ui.detail);
    ui.content_page.set_title(&format!("Since {from}"));
    let r = match r {
        Ok(r) => r,
        Err(f) => {
            failure_page(ui, &format!("{from} couldn't be compared"), f);
            return;
        }
    };
    ui.detail
        .append(&heading(&format!("What changed since {from}")));

    let choose = adw::PreferencesGroup::new();
    choose.add(&until_row(ui, from, to, all));
    let everything = adw::SwitchRow::builder()
        .title("Include package files and noise")
        .subtitle("Files package transactions wrote, caches, logs, access times")
        .active(all)
        .build();
    let ui3 = Rc::clone(ui);
    let (f, t) = (from.to_owned(), to.to_owned());
    everything.connect_active_notify(move |s| select_diff(&ui3, &f, &t, s.is_active()));
    choose.add(&everything);
    ui.detail.append(&choose);

    ui.detail
        .append(&facts_group(view::diff_facts(&r, local_full)));

    if !r.packages.is_empty() {
        let packages = adw::PreferencesGroup::new();
        let exp = adw::ExpanderRow::builder()
            .title(format!("Packages ({})", r.packages.len()))
            .expanded(r.packages.len() <= 8)
            .build();
        for p in &r.packages {
            exp.add_row(&action_row(&view::package_line(p), ""));
        }
        packages.add(&exp);
        ui.detail.append(&packages);
    }

    diff_files(ui, from, all, &r);
    ui.content.set_visible_child_name("detail");
}

/// A diff's changed files, by class, each leading to its cause.
fn diff_files(ui: &Rc<Ui>, from: &str, all: bool, r: &DiffReport) {
    let groups = view::diff_groups(r);
    if groups.is_empty() {
        let none = adw::StatusPage::builder()
            .icon_name("emblem-ok-symbolic")
            .title("No file changes")
            .description(if r.hidden > 0 {
                format!("{} package and noise changes are hidden", r.hidden)
            } else {
                "Nothing changed between these two".to_owned()
            })
            .build();
        ui.detail.append(&none);
    }
    for (title, rows) in groups {
        let group = adw::PreferencesGroup::builder()
            .title(format!("{title} ({})", rows.len()))
            .build();
        let few = rows.len() <= 5;
        for row in rows.iter().take(MAX_ROWS) {
            let w = file_row(&row.title, &row.subtitle, &row.summary, few);
            if let Some((id, _)) = row.cause {
                // A changed file leads to the cause that made it.
                let open = gtk::Button::builder()
                    .icon_name("go-next-symbolic")
                    .tooltip_text(format!("Open cause #{id}"))
                    .valign(gtk::Align::Center)
                    .build();
                open.add_css_class("flat");
                let ui3 = Rc::clone(ui);
                open.connect_clicked(move |_| select_cause(&ui3, id));
                if let Some(a) = w.downcast_ref::<adw::ActionRow>() {
                    a.add_suffix(&open);
                } else if let Some(e) = w.downcast_ref::<adw::ExpanderRow>() {
                    e.add_suffix(&open);
                }
            }
            group.add(&w);
        }
        if rows.len() > MAX_ROWS {
            group.add(&action_row(
                &format!(
                    "… and {} more (osreflog diff {from}.. lists them all)",
                    rows.len() - MAX_ROWS
                ),
                "",
            ));
        }
        ui.detail.append(&group);
    }
    if r.hidden > 0 && !all {
        let note = gtk::Label::builder()
            .label(format!("{} package and noise changes not listed", r.hidden))
            .xalign(0.0)
            .build();
        note.add_css_class("dim-label");
        ui.detail.append(&note);
    }
}
