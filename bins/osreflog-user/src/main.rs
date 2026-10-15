//! osreflog-user: the user-scope agent. GNOME settings (`GSettings`) live in dconf, and every
//! write goes through one daemon: the client calls `ca.desrt.dconf.Writer.Change` on the
//! session bus, dconf-service writes the database and replies with a tag, then emits
//! `Notify(prefix, keys, tag)`. This agent monitors those three messages, so each changed key
//! is credited to the process that asked, and compares values with a cache built from
//! `dconf dump /` at start.
//!
//! Changes go to the user's own ledger (`~/.local/state/osreflog/user.db`), never to the
//! system daemon: see ADR 0010.

mod dconf;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use futures_util::StreamExt;
use osreflog_ledger::Ledger;
use osreflog_model::{Actor, Cause, LiveChange};
use zbus::fdo::{DBusProxy, MonitoringProxy};
use zbus::message::Type;
use zbus::names::BusName;
use zbus::{MatchRule, MessageStream};

use crate::dconf::{Groups, ValueChange, Values};

const WRITER: &str = "ca.desrt.dconf.Writer";
const VIA: &str = "dconf-service (ca.desrt.dconf.Writer.Change)";
/// Calls and tags not matched within this long are dropped.
const KEEP_MS: i64 = 10_000;
const MAX_ARGS: usize = 16;
const MAX_ARG: usize = 200;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn proc_text(pid: u32, what: &str) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/{what}")).unwrap_or_default()
}

/// The process behind a call, read while it waits for its reply.
fn actor_of(pid: u32) -> Actor {
    let uid = proc_text(pid, "status")
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|l| l.split_whitespace().next()?.parse().ok())
        .unwrap_or(u32::MAX);
    let cgroup = proc_text(pid, "cgroup")
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or_default()
        .to_owned();
    let args: Vec<String> = std::fs::read(format!("/proc/{pid}/cmdline"))
        .unwrap_or_default()
        .split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .take(MAX_ARGS)
        .map(|a| String::from_utf8_lossy(a).chars().take(MAX_ARG).collect())
        .collect();
    let command = osreflog_model::redact::args(&args);
    Actor {
        pid,
        comm: proc_text(pid, "comm").trim().to_owned(),
        exe: std::fs::read_link(format!("/proc/{pid}/exe"))
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        uid,
        cgroup,
        command,
        via: Some(VIA.to_owned()),
    }
}

const SHELLS: &[&str] = &[
    "bash", "zsh", "fish", "sh", "dash", "ksh", "tcsh", "csh", "nu", "xonsh", "elvish",
];

/// Whether a shell started this process (directly or not): then a person typed a command,
/// and the app whose cgroup it runs in (a terminal) isn't the cause.
fn from_shell(pid: u32) -> bool {
    let mut p = pid;
    for _ in 0..32 {
        let stat = proc_text(p, "stat");
        // "pid (comm) state ppid ...": comm may contain spaces and parentheses.
        let Some((head, rest)) = stat.rsplit_once(") ") else {
            return false;
        };
        let comm = head.split_once(" (").map_or("", |(_, c)| c);
        if p != pid && SHELLS.contains(&comm) {
            return true;
        }
        match rest.split_whitespace().nth(1).and_then(|v| v.parse().ok()) {
            Some(parent) if parent > 1 => p = parent,
            _ => return false,
        }
    }
    false
}

/// Who asked: the process, and whether it came from a shell.
#[derive(Clone)]
struct Who {
    actor: Actor,
    from_shell: bool,
}

fn unknown_actor() -> Actor {
    Actor {
        pid: 0,
        comm: "?".into(),
        exe: String::new(),
        uid: u32::MAX,
        cgroup: String::new(),
        command: Vec::new(),
        via: Some(VIA.to_owned()),
    }
}

fn dconf(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("dconf")
        .args(args)
        .output()
        .context("running dconf")?;
    if !out.status.success() {
        bail!(
            "dconf {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The current values under `path`: one key, or a directory ending in `/`.
fn current(path: &str) -> anyhow::Result<Values> {
    if path.ends_with('/') {
        return Ok(dconf::parse_dump(path, &dconf(&["dump", path])?));
    }
    let v = dconf(&["read", path])?;
    let v = v.trim_end_matches('\n');
    Ok(if v.is_empty() {
        Values::new()
    } else {
        Values::from([(path.to_owned(), v.to_owned())])
    })
}

#[derive(Clone)]
struct Asker {
    ts_ms: i64,
    who: Who,
}

fn store(ledger: &mut Ledger, g: &dconf::Group<Who>) -> anyhow::Result<i64> {
    let actor = &g.actor.actor;
    let app = dconf::app_of_cgroup(&actor.cgroup).filter(|_| !g.actor.from_shell);
    let (kind, label) = match app {
        Some(app) => ("app", app),
        None if !actor.command.is_empty() => (
            "command",
            actor.command.join(" ").chars().take(MAX_ARG).collect(),
        ),
        None => ("unknown", actor.comm.clone()),
    };
    let cause = Cause {
        id: 0,
        kind: kind.to_owned(),
        label,
        actor: actor.clone(),
        started_ms: g.started_ms,
        ended_ms: g.last_ms,
        changes: g.changes.len() as u64,
        package_paths: 0,
    };
    let path = |c: &ValueChange| format!("dconf:{}", c.key);
    let changes: Vec<LiveChange> = g
        .changes
        .iter()
        .map(|(ts, c)| LiveChange {
            ts_ms: *ts,
            path: path(c),
            op: c.op().to_owned(),
            from: None,
            kind: "dconf".to_owned(),
            blob: None,
            size: None,
            actor: Some(format!("{} (pid {})", actor.comm, actor.pid)),
            target: None,
            summary: Vec::new(),
        })
        .collect();
    let id = ledger.store_cause(&cause, &changes)?;
    let summaries: Vec<(String, Vec<String>)> = g
        .changes
        .iter()
        .map(|(_, c)| (path(c), vec![c.summary()]))
        .collect();
    ledger.store_summaries(id, &summaries)?;
    // Raw values, for `osreflog undo --user`; a secret's value is never kept.
    let values: Vec<osreflog_ledger::DconfValue> = g
        .changes
        .iter()
        .filter(|(_, c)| !dconf::is_secret_key(&c.key))
        .map(|(_, c)| (c.key.clone(), c.old.clone(), c.new.clone()))
        .collect();
    ledger.store_dconf_values(id, &values)?;
    Ok(id)
}

async fn pid_of(dbus: &DBusProxy<'_>, name: &str) -> Option<u32> {
    let name = BusName::try_from(name).ok()?;
    dbus.get_connection_unix_process_id(name).await.ok()
}

async fn run() -> anyhow::Result<()> {
    let path = osreflog_ledger::user_ledger_path().context("no HOME or XDG_STATE_HOME")?;
    if let Some(dir) = path.parent() {
        // The values are the user's own settings: nobody else reads them.
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut ledger = Ledger::open(&path).with_context(|| format!("opening {}", path.display()))?;
    let mut cache = dconf::parse_dump("/", &dconf(&["dump", "/"])?);

    let lookup = zbus::Connection::session().await?;
    let dbus = DBusProxy::new(&lookup).await?;
    let monitor = zbus::connection::Builder::session()?.build().await?;
    let rules = [
        MatchRule::builder()
            .msg_type(Type::MethodCall)
            .interface(WRITER)?
            .member("Change")?
            .build(),
        MatchRule::builder()
            .msg_type(Type::Signal)
            .interface(WRITER)?
            .member("Notify")?
            .build(),
        // The reply carries the tag that ties a call to its Notify. Replies can't be matched
        // by sender here, so they're filtered against pending calls below.
        MatchRule::builder().msg_type(Type::MethodReturn).build(),
    ];
    MonitoringProxy::new(&monitor)
        .await?
        .become_monitor(&rules, 0)
        .await?;
    eprintln!(
        "watching dconf writes ({} keys cached), ledger {}",
        cache.len(),
        path.display()
    );

    let mut calls: HashMap<(String, u32), Asker> = HashMap::new();
    let mut tags: HashMap<String, Asker> = HashMap::new();
    let mut groups: Groups<Who> = Groups::default();
    let mut stream = MessageStream::from(&monitor);
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(msg) = msg else { bail!("the session bus closed the monitor") };
                let Ok(msg) = msg else { continue };
                let header = msg.header();
                match msg.message_type() {
                    Type::MethodCall => {
                        let Some(sender) = header.sender() else { continue };
                        let sender = sender.to_string();
                        let serial = header.primary().serial_num().get();
                        if let Some(pid) = pid_of(&dbus, &sender).await {
                            calls.insert((sender, serial), Asker { ts_ms: now_ms(), who: Who { actor: actor_of(pid), from_shell: from_shell(pid) } });
                        }
                    }
                    Type::MethodReturn => {
                        let (Some(dest), Some(reply)) = (header.destination(), header.reply_serial()) else { continue };
                        if let Some(asker) = calls.remove(&(dest.to_string(), reply.get()))
                            && let Ok(tag) = msg.body().deserialize::<String>()
                        {
                            tags.insert(tag, asker);
                        }
                    }
                    Type::Signal => {
                        let Ok((prefix, keys, tag)) = msg.body().deserialize::<(String, Vec<String>, String)>() else { continue };
                        let asker = tags.remove(&tag).unwrap_or_else(|| Asker { ts_ms: now_ms(), who: Who { actor: unknown_actor(), from_shell: false } });
                        let ts = now_ms();
                        for p in dconf::notified_paths(&prefix, &keys) {
                            match current(&p) {
                                Ok(now) => {
                                    let changes = dconf::apply(&mut cache, &p, &now);
                                    groups.add(asker.who.actor.pid, asker.who.clone(), ts, changes);
                                }
                                Err(e) => eprintln!("reading {p}: {e:#}"),
                            }
                        }
                    }
                    Type::Error => {
                        if let (Some(dest), Some(reply)) = (header.destination(), header.reply_serial()) {
                            calls.remove(&(dest.to_string(), reply.get()));
                        }
                    }
                }
            }
            _ = tick.tick() => {
                let now = now_ms();
                calls.retain(|_, a| now - a.ts_ms < KEEP_MS);
                tags.retain(|_, a| now - a.ts_ms < KEEP_MS);
                for g in groups.close(now) {
                    let (n, label) = (g.changes.len(), g.actor.actor.comm.clone());
                    match store(&mut ledger, &g) {
                        Ok(id) => eprintln!("#{id}: {n} dconf change(s) by {label}"),
                        Err(e) => eprintln!("storing changes by {label}: {e:#}"),
                    }
                }
            }
        }
    }
}

fn main() -> std::process::ExitCode {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("osreflog-user: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match rt.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("osreflog-user: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
