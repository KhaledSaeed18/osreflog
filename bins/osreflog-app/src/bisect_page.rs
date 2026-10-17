//! The bisect wizard (P8d): the sidebar's Bisect view, the form that starts one (a test
//! decides, or the person does: containers, VMs, or reboots of this machine), and the session
//! page that follows it: the probe that waits, the verdict buttons, the reboot into a metal
//! probe, what was found.

use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use osreflog_bisect::Status;

use crate::backend::{Failure, Request};
use crate::view::{self, BisectForm, TierChoice};
use crate::{
    Selected, Ui, action_row, button_row, clear, facts_group, file_row, local_full, page_header,
    reveal, toast,
};

/// Rebuilds the sidebar's Bisect view when the session changed.
pub fn sidebar(ui: &Rc<Ui>, s: Option<&Status>) {
    if ui.bisect_status.borrow().as_ref() == s && ui.bisect_side.first_child().is_some() {
        return;
    }
    *ui.bisect_status.borrow_mut() = s.cloned();
    clear(&ui.bisect_side);
    *ui.bisect_row.borrow_mut() = None;
    if let Some(s) = s {
        let group = adw::PreferencesGroup::builder()
            .title(if s.running {
                "Current bisect"
            } else {
                "Last bisect"
            })
            .build();
        let row = action_row(&view::session_title(s), &view::session_state(s));
        row.set_title_lines(2);
        row.set_subtitle_lines(2);
        row.set_activatable(true);
        row.add_prefix(&gtk::Image::from_icon_name(if s.waiting.is_some() {
            "dialog-question-symbolic"
        } else if s.running {
            "content-loading-symbolic"
        } else {
            "object-select-symbolic"
        }));
        let ui3 = Rc::clone(ui);
        row.connect_activated(move |_| select_session(&ui3));
        group.add(&row);
        ui.bisect_side.append(&group);
        *ui.bisect_row.borrow_mut() = Some(row);
    }
    let intro = gtk::Label::builder()
        .label(
            "A bisect finds the change that broke something: it tries copies of this system \
             between a checkpoint where things worked and one where they don't, and narrows \
             down to the cause. A test can decide each copy, or you can.",
        )
        .wrap(true)
        .xalign(0.0)
        .build();
    intro.add_css_class("dim-label");
    ui.bisect_side.append(&intro);
    let new = gtk::Button::with_label("New bisect…");
    new.add_css_class("pill");
    new.add_css_class("suggested-action");
    new.set_halign(gtk::Align::Start);
    new.set_sensitive(!s.is_some_and(|s| s.running));
    let ui3 = Rc::clone(ui);
    new.connect_clicked(move |_| show_form(&ui3));
    ui.bisect_side.append(&new);
    crate::highlight(ui);
}

/// The answer to `BisectStatus` (also sent after every bisect action).
pub fn on_status(ui: &Rc<Ui>, r: Result<Option<Box<Status>>, Failure>) {
    let Ok(s) = r else { return };
    let s = s.map(|b| *b);
    sidebar(ui, s.as_ref());
    if *ui.selected.borrow() == Some(Selected::Bisect) {
        let changed = ui.shown_session.borrow().as_ref() != s.as_ref();
        if changed {
            render_session(ui, s.as_ref());
        }
    }
}

/// A bisect action the daemon refused (or polkit did): said in a dialog.
pub fn refused(ui: &Rc<Ui>, what: &str, f: &Failure) {
    let title = match what {
        "start" => "The bisect didn't start",
        "verdict" => "The verdict wasn't taken",
        "rearm" => "The probe wasn't armed again",
        "reset" => "The bisect wasn't stopped",
        "undo-verdict" => "The verdict wasn't taken back",
        _ => "That didn't work",
    };
    let dialog = adw::AlertDialog::new(Some(title), Some(&f.message));
    dialog.add_response("close", "Close");
    dialog.present(Some(&ui.window));
}

pub fn select_session(ui: &Rc<Ui>) {
    *ui.selected.borrow_mut() = Some(Selected::Bisect);
    crate::highlight(ui);
    *ui.shown_session.borrow_mut() = None;
    let s = ui.bisect_status.borrow().clone();
    render_session(ui, s.as_ref());
    let _ = ui.requests.try_send(Request::BisectStatus);
}

fn combo(title: &str, choices: &[String], selected: usize) -> adw::ComboRow {
    let strings: Vec<&str> = choices.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&strings))
        .build();
    row.set_selected(u32::try_from(selected).unwrap_or(0));
    row
}

fn switch(title: &str, subtitle: &str) -> adw::SwitchRow {
    adw::SwitchRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build()
}

// The form's fields in one place: long, but flat.
#[allow(clippy::too_many_lines)]
pub fn show_form(ui: &Rc<Ui>) {
    *ui.selected.borrow_mut() = Some(Selected::BisectForm);
    crate::highlight(ui);
    crate::clear_detail(ui);
    ui.content_page.set_title("New bisect");
    ui.detail.append(&page_header(
        "edit-find-symbolic",
        None,
        "New bisect",
        "Find the change that broke something",
        None,
    ));

    let names = ui.names.borrow().clone();
    if names.len() < 2 {
        let none = adw::StatusPage::builder()
            .icon_name("bookmark-new-symbolic")
            .title("Two checkpoints needed")
            .description("A bisect runs between a checkpoint where things worked and a newer one where they don't. Take one with  sudo osreflog checkpoint.")
            .build();
        ui.detail.append(&none);
        reveal(ui, true);
        return;
    }
    let ends = adw::PreferencesGroup::builder().title("Between").build();
    let good = combo("Good: things still worked here", &names, 1);
    let bad = combo("Bad: broken here", &names, 0);
    ends.add(&good);
    ends.add(&bad);
    ui.detail.append(&ends);

    let how = adw::PreferencesGroup::builder()
        .title("Each copy is judged by")
        .build();
    let who = combo(
        "Who decides",
        &[
            "A test (automatic)".to_owned(),
            "Me (I look at each copy)".to_owned(),
        ],
        0,
    );
    let tier = combo(
        "Copies run as",
        &[
            "Whatever the test needs".to_owned(),
            "Containers (seconds)".to_owned(),
            "VMs with their own kernel".to_owned(),
            "Reboots of this machine (metal)".to_owned(),
        ],
        0,
    );
    let kernel = switch(
        "Needs the kernel",
        "Modules, drivers, early boot: a VM at least",
    );
    let hardware = switch(
        "Needs this machine's hardware",
        "Wi-Fi, GPU, suspend: reboots into each copy, you decide",
    );
    let boot = switch(
        "Boot systemd in the container",
        "For a test that needs services running",
    );
    how.add(&who);
    how.add(&tier);
    how.add(&kernel);
    how.add(&hardware);
    how.add(&boot);
    ui.detail.append(&how);

    let test_group = adw::PreferencesGroup::builder()
        .title("Test")
        .description(
            "Run as root inside each copy. Exit 0: good. 125: can't tell (skipped). 1–127: bad.",
        )
        .build();
    let test = adw::EntryRow::builder().title("Command").build();
    test_group.add(&test);
    ui.detail.append(&test_group);
    let sync_test = {
        let test_group = test_group.clone();
        let boot = boot.clone();
        move |manual: bool| {
            test_group.set_visible(!manual);
            boot.set_visible(!manual);
        }
    };
    sync_test(false);
    who.connect_selected_notify(move |w| sync_test(w.selected() == 1));

    let error = gtk::Label::builder().wrap(true).xalign(0.0).build();
    error.add_css_class("error");
    ui.detail.append(&error);
    let start = gtk::Button::with_label("Start");
    start.add_css_class("pill");
    start.add_css_class("suggested-action");
    let ui3 = Rc::clone(ui);
    start.connect_clicked(move |_| {
        let pick = |row: &adw::ComboRow| {
            names
                .get(usize::try_from(row.selected()).unwrap_or(0))
                .cloned()
                .unwrap_or_default()
        };
        let form = BisectForm {
            good: pick(&good),
            bad: pick(&bad),
            manual: who.selected() == 1,
            tier: match tier.selected() {
                1 => TierChoice::Nspawn,
                2 => TierChoice::Vm,
                3 => TierChoice::Metal,
                _ => TierChoice::Auto,
            },
            needs_kernel: kernel.is_active(),
            needs_hardware: hardware.is_active(),
            boot: boot.is_active(),
            test: test.text().to_string(),
        };
        match view::bisect_args(&form) {
            Ok(args) => {
                error.set_label("");
                *ui3.selected.borrow_mut() = Some(Selected::Bisect);
                *ui3.pending.borrow_mut() = Some(Selected::Bisect);
                *ui3.shown_session.borrow_mut() = None;
                ui3.busy.set_title("Starting the bisect…");
                ui3.busy.set_description(Some(
                    "Your system asks for an administrator's password: a bisect builds copies of it.",
                ));
                ui3.content.set_visible_child_name("busy");
                let _ = ui3.requests.try_send(Request::BisectRun(Box::new(args)));
            }
            Err(e) => error.set_label(&e),
        }
    });
    let buttons = button_row();
    buttons.append(&start);
    ui.detail.append(&buttons);
    reveal(ui, true);
}

fn pill(label: &str, class: Option<&str>) -> gtk::Button {
    let b = gtk::Button::with_label(label);
    b.add_css_class("pill");
    if let Some(c) = class {
        b.add_css_class(c);
    }
    b
}

/// Reboots, after asking: the armed metal probe boots once, the boot after it is the default.
fn confirm_reboot(ui: &Rc<Ui>, probe: u32) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Reboot into probe {probe}?")),
        Some(
            "This machine restarts once into a throwaway copy of itself (its /home too). Check \
             what you are bisecting there, give the verdict with  sudo osreflog-verdict good, bad \
             or skip, and reboot: the boot after it is your normal system, and osreflog goes on.",
        ),
    );
    dialog.add_responses(&[("cancel", "Not now"), ("reboot", "Reboot")]);
    dialog.set_response_appearance("reboot", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let ui3 = Rc::clone(ui);
    dialog.connect_response(None, move |_, response| {
        if response != "reboot" {
            return;
        }
        let started = gio::Subprocess::newv(
            &[
                std::ffi::OsStr::new("systemctl"),
                std::ffi::OsStr::new("reboot"),
            ],
            gio::SubprocessFlags::NONE,
        );
        if let Err(e) = started {
            refused(
                &ui3,
                "reboot",
                &Failure {
                    down: false,
                    message: format!("systemctl reboot: {e}"),
                },
            );
        }
    });
    dialog.present(Some(&ui.window));
}

fn confirm_reset(ui: &Rc<Ui>, running: bool) {
    if !running {
        let _ = ui.requests.try_send(Request::BisectReset);
        return;
    }
    let dialog = adw::AlertDialog::new(
        Some("Stop this bisect?"),
        Some(
            "The probe that waits, its copies and the session are removed. What was found so far is lost.",
        ),
    );
    dialog.add_responses(&[("cancel", "Keep it"), ("stop", "Stop")]);
    dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
    dialog.set_close_response("cancel");
    let ui3 = Rc::clone(ui);
    dialog.connect_response(None, move |_, response| {
        if response == "stop" {
            let _ = ui3.requests.try_send(Request::BisectReset);
        }
    });
    dialog.present(Some(&ui.window));
}

// The session page in one place: long, but flat.
#[allow(clippy::too_many_lines)]
fn render_session(ui: &Rc<Ui>, s: Option<&Status>) {
    // A new page fades in; the same session refreshed (a probe done, a verdict taken) doesn't.
    let fresh = ui
        .shown_session
        .replace(s.cloned())
        .is_none_or(|old| s.is_none_or(|s| s.started_ms != old.started_ms));
    crate::clear_detail(ui);
    let Some(s) = s else {
        ui.content_page.set_title("Bisect");
        let none = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title("No bisect")
            .description("Start one from the Bisect view.")
            .build();
        ui.detail.append(&none);
        reveal(ui, true);
        return;
    };
    ui.content_page.set_title("Bisect");
    let (icon, tone) = session_icon(s);
    ui.detail.append(&page_header(
        icon,
        tone,
        &view::session_title(s),
        &header_line(s),
        None,
    ));

    if let Some(w) = &s.waiting {
        let group = adw::PreferencesGroup::builder()
            .title(format!("Probe {}: {}", w.probe, w.what))
            .description(format!("waiting since {}", local_full(w.since_ms)))
            .build();
        for line in &w.how {
            let row = action_row(line, "");
            row.set_title_selectable(true);
            group.add(&row);
        }
        ui.detail.append(&group);
        let buttons = button_row();
        if w.reboot {
            let reboot = pill("Reboot now…", Some("suggested-action"));
            let ui3 = Rc::clone(ui);
            let probe = w.probe;
            reboot.connect_clicked(move |_| confirm_reboot(&ui3, probe));
            buttons.append(&reboot);
        } else if s.args.tier == osreflog_bisect::Tier::Metal {
            let rearm = pill("Arm it again", None);
            let ui3 = Rc::clone(ui);
            rearm.connect_clicked(move |b| {
                b.set_sensitive(false);
                toast(&ui3, "Arming the probe again…");
                let _ = ui3.requests.try_send(Request::BisectRearm);
            });
            buttons.append(&rearm);
        }
        for (label, verdict, class) in [
            ("Good", "good", Some("success")),
            ("Bad", "bad", Some("destructive-action")),
            ("Skip", "skip", None),
        ] {
            let b = pill(label, class);
            b.set_tooltip_text(Some(match verdict {
                "good" => "It works in this copy",
                "bad" => "It's broken in this copy",
                _ => "This copy can't tell",
            }));
            let ui3 = Rc::clone(ui);
            let row = buttons.clone();
            b.connect_clicked(move |_| {
                // One verdict per probe: a second click could land on the next probe.
                row.set_sensitive(false);
                toast(&ui3, &format!("Verdict sent: {label}"));
                let _ = ui3
                    .requests
                    .try_send(Request::BisectVerdict(verdict.to_owned()));
            });
            buttons.append(&b);
        }
        ui.detail.append(&buttons);
    }

    let actions = button_row();
    if let Some((title, lines, open)) = view::finding(s) {
        let group = adw::PreferencesGroup::builder().title(&title).build();
        for line in lines.iter().filter(|l| !l.is_empty()) {
            let row = action_row(line, "");
            row.set_title_selectable(true);
            group.add(&row);
        }
        ui.detail.append(&group);
        if let Some(id) = open {
            let b = pill(&format!("Open #{id}"), Some("suggested-action"));
            let ui3 = Rc::clone(ui);
            b.connect_clicked(move |_| crate::select_cause(&ui3, id));
            actions.append(&b);
        }
    }

    if s.args.manual && !s.probes.is_empty() {
        let last = s.probes.last().map(|p| p.what.clone()).unwrap_or_default();
        let back = pill("Take back the last verdict", None);
        back.set_tooltip_text(Some(&format!(
            "A mistake? The verdict for “{last}” is forgotten and that probe is asked again{}",
            if s.running {
                ""
            } else {
                ": the bisect goes on"
            }
        )));
        let ui3 = Rc::clone(ui);
        back.connect_clicked(move |b| {
            b.set_sensitive(false);
            toast(&ui3, "Taking back the last verdict…");
            let _ = ui3.requests.try_send(Request::BisectUndoVerdict);
        });
        actions.append(&back);
    }
    if actions.first_child().is_some() {
        ui.detail.append(&actions);
    }

    let probes = view::probe_rows(s);
    if !probes.is_empty() {
        let group = adw::PreferencesGroup::builder()
            .title(format!("Probes ({})", probes.len()))
            .build();
        for (icon, title, subtitle, output) in &probes {
            let lines: Vec<String> = if output.is_empty() {
                vec![]
            } else {
                vec![output.clone()]
            };
            let w = file_row(title, subtitle, &lines, false);
            let image = gtk::Image::from_icon_name(icon);
            image.add_css_class(match *icon {
                "object-select-symbolic" => "success",
                "process-stop-symbolic" => "error",
                _ => "dim-label",
            });
            if let Some(a) = w.downcast_ref::<adw::ActionRow>() {
                a.add_prefix(&image);
            } else if let Some(e) = w.downcast_ref::<adw::ExpanderRow>() {
                e.add_prefix(&image);
            }
            group.add(&w);
        }
        ui.detail.append(&group);
    }

    if !s.log.is_empty() {
        let group = adw::PreferencesGroup::new();
        group.add(&file_row(
            "Log",
            &format!("{} lines", s.log.len()),
            &[s.log.join("\n")],
            false,
        ));
        ui.detail.append(&group);
    }
    ui.detail
        .append(&facts_group(vec![("Started", local_full(s.started_ms))]));

    let reset = pill(
        if s.running {
            "Stop this bisect…"
        } else {
            "Forget it"
        },
        None,
    );
    let ui3 = Rc::clone(ui);
    let running = s.running;
    reset.connect_clicked(move |_| confirm_reset(&ui3, running));
    let buttons = button_row();
    buttons.append(&reset);
    ui.detail.append(&buttons);
    reveal(ui, fresh);
}

/// The line under a session's title: its state, but a found cause only by its number, since
/// the finding below says what it is.
fn header_line(s: &Status) -> String {
    match &s.finding {
        Some(osreflog_bisect::Finding::Cause { culprit, .. }) if s.error.is_none() => format!(
            "Found #{} in {} probe{}",
            culprit.id,
            s.probes.len(),
            if s.probes.len() == 1 { "" } else { "s" }
        ),
        _ => view::session_state(s),
    }
}

/// The icon at the head of a session's page, and its tone: waiting on you, running, found.
fn session_icon(s: &Status) -> (&'static str, Option<&'static str>) {
    use osreflog_bisect::Finding;
    if s.waiting.is_some() {
        ("dialog-question-symbolic", Some("warning"))
    } else if s.running {
        ("edit-find-symbolic", None)
    } else if s.error.is_some() {
        ("dialog-warning-symbolic", Some("warning"))
    } else if matches!(
        s.finding,
        Some(Finding::Cause { .. } | Finding::Checkpoint { .. })
    ) {
        ("object-select-symbolic", Some("success"))
    } else {
        ("edit-find-symbolic", None)
    }
}

/// While a session runs and its page is open, ask for it every second.
pub fn tick(ui: &Rc<Ui>) -> glib::ControlFlow {
    let following = *ui.selected.borrow() == Some(Selected::Bisect)
        && ui
            .bisect_status
            .borrow()
            .as_ref()
            .is_some_and(|s| s.running);
    if following && ui.requests.try_send(Request::BisectStatus).is_err() {
        return glib::ControlFlow::Break;
    }
    glib::ControlFlow::Continue
}
