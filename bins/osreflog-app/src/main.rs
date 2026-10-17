//! osreflog for GNOME: the history of this system, cause by cause (P8a), and what changed
//! between two checkpoints (P8b). Read-only: it asks osreflogd over the system bus, which allows
//! reading without a password.

mod backend;
mod bisect_page;
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
    Diff {
        from: String,
        to: String,
        all: bool,
    },
    /// The undo of a cause: its preview, then its result.
    Undo(i64),
    /// The bisect form, and the session it starts.
    BisectForm,
    Bisect,
}

/// What a list shows now (its keys and the day it was built on), so an unchanged refresh
/// doesn't rebuild it.
type Shown<T> = RefCell<Option<(Vec<T>, String)>>;

/// The widgets the reply loop updates.
struct Ui {
    window: adw::ApplicationWindow,
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
    /// The causes as last read, and the search over them.
    causes: RefCell<Vec<osreflog_model::Cause>>,
    search: gtk::SearchEntry,
    shown_checkpoints: Shown<(String, bool)>,
    /// The sidebar's Bisect view, the session as last read, and as the session page shows it.
    bisect_side: gtk::Box,
    bisect_status: RefCell<Option<osreflog_bisect::Status>>,
    shown_session: RefCell<Option<osreflog_bisect::Status>>,
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
    let bisect_side = page_box();
    let views = adw::ViewStack::new();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search causes")
        .margin_top(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    let causes_view = gtk::Box::new(gtk::Orientation::Vertical, 0);
    causes_view.append(&search);
    causes_view.append(&scrolled(&timeline));
    views.add_titled_with_icon(
        &causes_view,
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
    views.add_titled_with_icon(
        &scrolled(&bisect_side),
        Some("bisect"),
        "Bisect",
        "edit-find-symbolic",
    );
    let switcher = adw::ViewSwitcher::builder()
        .stack(&views)
        .policy(adw::ViewSwitcherPolicy::Narrow)
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
    split.set_min_sidebar_width(380.0);
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
        window: window.clone(),
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
        causes: RefCell::new(Vec::new()),
        search: search.clone(),
        shown_checkpoints: RefCell::new(None),
        bisect_side,
        bisect_status: RefCell::new(None),
        shown_session: RefCell::new(None),
    });

    let ui4 = Rc::clone(&ui);
    ui.search.connect_search_changed(move |_| {
        let causes = ui4.causes.borrow().clone();
        show_causes(&ui4, &causes, &day_of(0));
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
                Reply::Bisect(r) => bisect_page::on_status(&ui2, r),
                Reply::BisectRefused(what, f) => bisect_page::refused(&ui2, &what, &f),
                Reply::UndoPlan(id, r) => {
                    if *ui2.selected.borrow() == Some(Selected::Undo(id)) {
                        show_undo_plan(&ui2, id, r);
                    }
                }
                Reply::Undone(id, r) => {
                    if *ui2.selected.borrow() == Some(Selected::Undo(id)) {
                        show_undone(&ui2, id, r);
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
    // Development: open a page at start, `since:c000134`, `cause:765`, `search:<text>`, `undo:765`, `bisect:session`
    // or `bisect:new` (input through GTK's
    // Broadway backend, used to look at the window from a browser, drops clicks).
    if let Ok(open) = std::env::var("OSREFLOG_APP_OPEN") {
        match open.split_once(':') {
            Some(("since", name)) => select_diff(&ui, name, NOW, false),
            Some(("search", query)) => ui.search.set_text(query),
            Some(("cause", id)) => {
                if let Ok(id) = id.parse() {
                    select_cause(&ui, id);
                }
            }
            Some(("bisect", "session")) => bisect_page::select_session(&ui),
            Some(("bisect", "new")) => {
                // The form needs the checkpoint names: build it once the first history is in.
                let ui5 = Rc::clone(&ui);
                glib::timeout_add_seconds_local_once(2, move || bisect_page::show_form(&ui5));
            }
            Some(("undo", id)) => {
                if let Ok(id) = id.parse() {
                    select_undo(&ui, id);
                }
            }
            _ => eprintln!(
                "osreflog-app: OSREFLOG_APP_OPEN is since:<checkpoint>, cause:<id>, search:<text>, undo:<id>, bisect:session or bisect:new"
            ),
        }
    }
    let ui4 = Rc::clone(&ui);
    glib::timeout_add_seconds_local(1, move || bisect_page::tick(&ui4));
    let refresh = requests.clone();
    glib::timeout_add_seconds_local(REFRESH_S, move || {
        if refresh.try_send(Request::Refresh).is_err() {
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
    window.present();
}

/// Empties the content page. Focus leaves it first: a clicked button removed while focused
/// left GTK checking focus ancestry against a widget that was gone (`gtk_widget_is_ancestor:
/// assertion 'GTK_IS_WIDGET (widget)' failed`).
fn clear_detail(ui: &Ui) {
    gtk::prelude::GtkWindowExt::set_focus(&ui.window, None::<&gtk::Widget>);
    clear(&ui.detail);
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

fn show_history(ui: &Rc<Ui>, r: Result<Box<History>, Failure>) {
    let (status, causes, log, bisect) = match r {
        Ok(v) => *v,
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
            ui.title.set_tooltip_text(None);
            ui.sidebar.set_visible_child_name("down");
            *ui.shown_causes.borrow_mut() = None;
            *ui.shown_checkpoints.borrow_mut() = None;
            return;
        }
    };
    ui.title.set_subtitle(&view::status_short(&status));
    ui.title
        .set_tooltip_text(Some(&view::status_line(&status, local_full)));
    let today = day_of(0);
    show_causes(ui, &causes, &today);
    show_checkpoints(ui, &log, &today);
    bisect_page::sidebar(ui, bisect.as_ref());
    if *ui.selected.borrow() == Some(Selected::Bisect)
        && ui.shown_session.borrow().as_ref() != bisect.as_ref()
    {
        bisect_page::on_status(ui, Ok(bisect.map(Box::new)));
    }
    ui.sidebar.set_visible_child_name("views");
}

fn time_suffix(text: &str) -> gtk::Label {
    let time = gtk::Label::new(Some(text));
    time.add_css_class("dim-label");
    time.add_css_class("numeric");
    time
}

/// A list row on one line each for the title and the subtitle, the whole text in its tooltip:
/// long commands otherwise wrap to three lines.
fn list_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let r = action_row(title, subtitle);
    r.set_title_lines(1);
    r.set_subtitle_lines(1);
    r.set_tooltip_text(Some(&format!("{title}\n{subtitle}")));
    r
}

fn show_causes(ui: &Rc<Ui>, causes: &[osreflog_model::Cause], today: &str) {
    if ui.causes.borrow().as_slice() != causes {
        *ui.causes.borrow_mut() = causes.to_vec();
    }
    let query = ui.search.text().to_string();
    let found: Vec<_> = causes
        .iter()
        .filter(|c| view::matches(c, &query))
        .cloned()
        .collect();
    let key = (
        found.iter().map(|c| c.id).collect(),
        format!("{today}\n{query}"),
    );
    if ui.shown_causes.borrow().as_ref() == Some(&key) {
        return;
    }
    clear(&ui.timeline);
    let days = view::days(&found, local, today, &day_of(-1));
    if days.is_empty() && !query.trim().is_empty() {
        let empty = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title("No matching causes")
            .description("The search looks in each cause's label, kind, process and command.")
            .build();
        ui.timeline.append(&empty);
    } else if days.is_empty() {
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
            let r = list_row(&row.title, &row.subtitle);
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
            let r = list_row(&row.title, &row.subtitle);
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
    clear_detail(ui);
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
    let undo = gtk::Button::builder()
        .label("Undo…")
        .tooltip_text("See what undoing this cause would do; nothing changes until you confirm")
        .halign(gtk::Align::Start)
        .build();
    undo.add_css_class("pill");
    let ui3 = Rc::clone(ui);
    undo.connect_clicked(move |_| select_undo(&ui3, id));
    ui.detail.append(&undo);

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
    clear_detail(ui);
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
            .icon_name("object-select-symbolic")
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

/// Asks what undoing a cause would do. Nothing is written by this.
fn select_undo(ui: &Rc<Ui>, id: i64) {
    *ui.selected.borrow_mut() = Some(Selected::Undo(id));
    ui.busy
        .set_title(&format!("Checking what undoing #{id} would do…"));
    ui.busy
        .set_description(Some("Nothing changes until you confirm."));
    ui.content.set_visible_child_name("busy");
    ui.split.set_show_content(true);
    let _ = ui.requests.try_send(Request::UndoPlan(id));
}

fn verdict_icon(verdict: &str) -> &'static str {
    match verdict {
        "restore" => "edit-undo-symbolic",
        "same" => "object-select-symbolic",
        "conflict" => "dialog-warning-symbolic",
        _ => "action-unavailable-symbolic",
    }
}

fn show_undo_plan(ui: &Rc<Ui>, id: i64, r: Result<Box<osreflog_undo::Preview>, Failure>) {
    clear_detail(ui);
    ui.content_page.set_title(&format!("Undo #{id}"));
    let p = match r {
        Ok(p) => p,
        Err(f) => {
            failure_page(
                ui,
                &format!("What undoing #{id} would do couldn't be worked out"),
                f,
            );
            return;
        }
    };
    ui.detail
        .append(&heading(&format!("Undo #{} {}", p.cause.id, p.cause.label)));
    let summary = gtk::Label::builder()
        .label(view::undo_summary(&p))
        .wrap(true)
        .xalign(0.0)
        .selectable(true)
        .build();
    ui.detail.append(&summary);

    let rows = view::undo_rows(&p);
    if !rows.is_empty() {
        let group = adw::PreferencesGroup::builder()
            .title(format!("Paths ({})", rows.len()))
            .build();
        for row in &rows {
            let w = match &row.diff {
                Some(d) => file_row(
                    &row.path,
                    &row.what,
                    std::slice::from_ref(d),
                    rows.len() <= 5,
                ),
                None => file_row(&row.path, &row.what, &[], false),
            };
            let icon = gtk::Image::from_icon_name(verdict_icon(row.verdict));
            if let Some(a) = w.downcast_ref::<adw::ActionRow>() {
                a.add_prefix(&icon);
            } else if let Some(e) = w.downcast_ref::<adw::ExpanderRow>() {
                e.add_prefix(&icon);
            }
            group.add(&w);
        }
        ui.detail.append(&group);
    }
    if !p.skipped.is_empty() {
        let group = adw::PreferencesGroup::builder()
            .title("Left alone")
            .description("Paths of this cause undo never touches")
            .build();
        for s in &p.skipped {
            group.add(&action_row(&s.path, &s.reason));
        }
        ui.detail.append(&group);
    }

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let back = gtk::Button::with_label("Back to the cause");
    back.add_css_class("pill");
    let ui3 = Rc::clone(ui);
    back.connect_clicked(move |_| select_cause(&ui3, id));
    buttons.append(&back);
    let n = p.to_restore();
    if n > 0 && p.delegated.is_none() {
        let apply = gtk::Button::with_label(&format!(
            "Undo {n} change{}…",
            if n == 1 { "" } else { "s" }
        ));
        apply.add_css_class("pill");
        apply.add_css_class("destructive-action");
        let ui3 = Rc::clone(ui);
        let digest = p.digest.clone();
        apply.connect_clicked(move |_| confirm_undo(&ui3, id, n, &digest));
        buttons.append(&apply);
    }
    ui.detail.append(&buttons);
    ui.content.set_visible_child_name("detail");
}

/// The last word before anything is written: polkit then asks for the password.
fn confirm_undo(ui: &Rc<Ui>, id: i64, n: usize, digest: &str) {
    let dialog = adw::AlertDialog::new(
        Some(&format!(
            "Undo {n} change{}?",
            if n == 1 { "" } else { "s" }
        )),
        Some(
            "osreflog takes a checkpoint first. Each path is checked again as it is written: \
             anything that changed in the meantime is left alone. Replaced versions go to the \
             attic, and the undo is recorded as a cause of its own, so it can be undone too.",
        ),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("undo", "Undo")]);
    dialog.set_response_appearance("undo", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let ui3 = Rc::clone(ui);
    let digest = digest.to_owned();
    dialog.connect_response(None, move |_, response| {
        if response != "undo" {
            return;
        }
        *ui3.selected.borrow_mut() = Some(Selected::Undo(id));
        ui3.busy.set_title(&format!("Undoing #{id}…"));
        ui3.busy.set_description(Some(
            "Your system asks for an administrator's password: every undo needs it.",
        ));
        ui3.content.set_visible_child_name("busy");
        let _ = ui3.requests.try_send(Request::Undo {
            id,
            digest: digest.clone(),
        });
    });
    dialog.present(Some(&ui.window));
}

fn show_undone(ui: &Rc<Ui>, id: i64, r: Result<Box<osreflog_undo::Applied>, Failure>) {
    clear_detail(ui);
    let a = match r {
        Ok(a) => a,
        Err(f) => {
            failure_page(ui, &format!("#{id} wasn't undone"), f);
            let again = gtk::Button::with_label("Back to the preview");
            again.add_css_class("pill");
            again.set_halign(gtk::Align::Center);
            let ui3 = Rc::clone(ui);
            again.connect_clicked(move |_| select_undo(&ui3, id));
            ui.detail.append(&again);
            return;
        }
    };
    let rows = view::applied_rows(&a);
    let ok = rows.iter().filter(|(_, _, ok)| *ok).count();
    ui.detail.append(&heading(&format!(
        "Undone: #{id} ({ok} of {} paths)",
        rows.len()
    )));
    let mut facts = vec![(
        "Checkpoint first",
        format!("{} (verified before anything was written)", a.pre_undo.name),
    )];
    facts.push(("Replaced versions", a.report.attic.clone()));
    if a.reloaded_systemd {
        facts.push((
            "systemd",
            "reloaded: unit files or enablement links changed".into(),
        ));
    }
    ui.detail.append(&facts_group(facts));
    let group = adw::PreferencesGroup::builder().title("Paths").build();
    for (path, line, ok) in rows {
        let row = action_row(&path, &line);
        row.add_prefix(&gtk::Image::from_icon_name(if ok {
            "object-select-symbolic"
        } else {
            "dialog-warning-symbolic"
        }));
        group.add(&row);
    }
    ui.detail.append(&group);
    if let Some(cause) = a.undo_cause {
        let open = gtk::Button::with_label(&format!("Open #{cause}, the undo itself"));
        open.set_tooltip_text(Some("Undoing it puts the change back"));
        open.add_css_class("pill");
        open.set_halign(gtk::Align::Start);
        let ui3 = Rc::clone(ui);
        open.connect_clicked(move |_| select_cause(&ui3, cause));
        ui.detail.append(&open);
    }
    ui.content.set_visible_child_name("detail");
}
