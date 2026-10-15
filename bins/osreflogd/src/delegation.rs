//! Who asked a daemon to make a change. Many admin tools don't write files themselves: they
//! call a daemon over D-Bus (`timedatectl` → systemd-timedated, `firewall-cmd` → firewalld,
//! `systemctl enable` → pid 1). The live witness sees the daemon write; this module watches
//! method calls on the system bus so the write can be credited to the caller.
//!
//! A separate connection becomes a bus monitor. For each call that could change something,
//! the caller's pid is looked up at once (it waits for its reply, so it's alive) and the
//! destination owner's pid is looked up with retries (bus activation may still be starting
//! the daemon). The capture thread matches closed causes against these calls.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use zbus::fdo::{DBusProxy, MonitoringProxy};
use zbus::message::Type;
use zbus::names::BusName;
use zbus::{MatchRule, MessageStream};

use crate::capture::LiveState;
use crate::engine::now_ms;

/// How far back a call may precede the cause it explains.
pub const MATCH_WINDOW_MS: i64 = 10_000;
/// How long calls are kept.
const KEEP_MS: i64 = 30_000;
const MAX_CALLS: usize = 4096;
const OWNER_RETRIES_MS: &[u64] = &[0, 100, 300, 700, 1500];

/// Members that only read. Matched case-insensitively as prefixes; skipping them keeps a
/// daemon's own writes from being credited to whoever happened to query it.
const READ_PREFIXES: &[&str] = &[
    "get",
    "list",
    "introspect",
    "ping",
    "can",
    "is",
    "has",
    "check",
    "lookup",
    "find",
    "inspect",
    "describe",
    "query",
    "read",
    "watch",
    "subscribe",
    "register",
    "unregister",
    "addmatch",
    "removematch",
    "hello",
];
const IGNORED_DESTINATIONS: &[&str] = &[
    "org.freedesktop.DBus",
    "org.freedesktop.PolicyKit1",
    osreflog_model::bus::NAME,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    pub ts_ms: i64,
    pub client_pid: u32,
    pub owner_pid: Option<u32>,
    pub destination: String,
    pub method: String,
}

/// Recent calls, oldest first.
#[derive(Debug, Default)]
pub struct Calls {
    ring: VecDeque<Call>,
}

impl Calls {
    pub fn push(&mut self, call: Call) {
        while self.ring.len() >= MAX_CALLS {
            self.ring.pop_front();
        }
        self.ring.push_back(call);
    }

    /// Records the destination owner of a call already pushed.
    pub fn set_owner(&mut self, ts_ms: i64, client_pid: u32, method: &str, owner_pid: u32) {
        if let Some(c) = self
            .ring
            .iter_mut()
            .rev()
            .find(|c| c.ts_ms == ts_ms && c.client_pid == client_pid && c.method == method)
        {
            c.owner_pid = Some(owner_pid);
        }
    }

    /// The latest call to a daemon whose pid is `writer_pid`, made in the window before a
    /// cause that started at `started_ms` and ended at `ended_ms`. Calls the writer made to
    /// itself don't count.
    #[must_use]
    pub fn explaining(&self, writer_pid: u32, started_ms: i64, ended_ms: i64) -> Option<&Call> {
        self.ring
            .iter()
            .filter(|c| c.owner_pid == Some(writer_pid) && c.client_pid != writer_pid)
            .filter(|c| c.ts_ms >= started_ms - MATCH_WINDOW_MS && c.ts_ms <= ended_ms)
            .max_by_key(|c| c.ts_ms)
    }

    pub fn gc(&mut self, now_ms: i64) {
        while self
            .ring
            .front()
            .is_some_and(|c| now_ms - c.ts_ms > KEEP_MS)
        {
            self.ring.pop_front();
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

/// Whether a method call could change something (see [`READ_PREFIXES`]).
#[must_use]
pub fn may_write(destination: &str, member: &str) -> bool {
    if IGNORED_DESTINATIONS.contains(&destination) {
        return false;
    }
    let lower = member.to_ascii_lowercase();
    !READ_PREFIXES.iter().any(|p| lower.starts_with(p))
}

async fn pid_of(dbus: &DBusProxy<'_>, name: &str) -> Option<u32> {
    let name = BusName::try_from(name).ok()?;
    dbus.get_connection_unix_process_id(name).await.ok()
}

/// Watches the system bus until it fails; the caller logs and carries on without it.
///
/// # Errors
/// If the bus connection or `BecomeMonitor` fails.
pub async fn run(live: Arc<LiveState>) -> zbus::Result<()> {
    let lookup = zbus::Connection::system().await?;
    let dbus = DBusProxy::new(&lookup).await?;
    let monitor = zbus::connection::Builder::system()?.build().await?;
    let rule = MatchRule::builder().msg_type(Type::MethodCall).build();
    MonitoringProxy::new(&monitor)
        .await?
        .become_monitor(&[rule], 0)
        .await?;
    tracing::info!("watching D-Bus method calls for delegated writes");
    let mut stream = MessageStream::from(&monitor);
    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };
        let header = msg.header();
        let (Some(sender), Some(destination), Some(member)) =
            (header.sender(), header.destination(), header.member())
        else {
            continue;
        };
        let destination = destination.to_string();
        if !may_write(&destination, member.as_str()) {
            continue;
        }
        let method = match header.interface() {
            Some(i) => format!("{i}.{member}"),
            None => member.to_string(),
        };
        let sender = sender.to_string();
        let ts_ms = now_ms();
        let Some(client_pid) = pid_of(&dbus, &sender).await else {
            continue;
        };
        let dbus = dbus.clone();
        let live = Arc::clone(&live);
        live.calls().push(Call {
            ts_ms,
            client_pid,
            owner_pid: None,
            destination: destination.clone(),
            method: method.clone(),
        });
        tokio::spawn(async move {
            for wait in OWNER_RETRIES_MS {
                tokio::time::sleep(Duration::from_millis(*wait)).await;
                if let Some(owner) = pid_of(&dbus, &destination).await {
                    live.calls().set_owner(ts_ms, client_pid, &method, owner);
                    return;
                }
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(ts_ms: i64, client_pid: u32, owner_pid: Option<u32>, method: &str) -> Call {
        Call {
            ts_ms,
            client_pid,
            owner_pid,
            destination: "org.fedoraproject.FirewallD1".into(),
            method: method.into(),
        }
    }

    #[test]
    fn reads_are_skipped() {
        assert!(may_write("org.freedesktop.timedate1", "SetTimezone"));
        assert!(may_write("org.fedoraproject.FirewallD1", "addZone2"));
        assert!(may_write("org.freedesktop.systemd1", "EnableUnitFiles"));
        assert!(!may_write("org.freedesktop.NetworkManager", "GetDevices"));
        assert!(!may_write("org.fedoraproject.FirewallD1", "getZones"));
        assert!(!may_write("org.freedesktop.systemd1", "ListUnits"));
        assert!(!may_write("org.freedesktop.DBus", "AddMatch"));
        assert!(!may_write(
            "org.freedesktop.PolicyKit1",
            "CheckAuthorization"
        ));
    }

    #[test]
    fn matches_the_latest_call_to_the_writer_in_the_window() {
        let mut calls = Calls::default();
        calls.push(call(1_000, 50, Some(900), "config.addZone2"));
        calls.push(call(9_000, 51, Some(900), "config.addZone2"));
        calls.push(call(9_500, 52, Some(777), "SetTimezone"));
        calls.push(call(9_600, 900, Some(900), "selfCall"));
        let m = calls.explaining(900, 10_000, 10_500).unwrap();
        assert_eq!(m.client_pid, 51, "latest call to pid 900 before the cause");
        assert!(
            calls.explaining(900, 30_000, 30_100).is_none(),
            "outside the window"
        );
        assert!(
            calls.explaining(123, 10_000, 10_500).is_none(),
            "nobody asked pid 123"
        );
        calls.gc(40_000);
        assert!(calls.is_empty());
    }

    #[test]
    fn owner_resolved_later() {
        let mut calls = Calls::default();
        calls.push(call(5_000, 60, None, "SetTimezone"));
        assert!(calls.explaining(1, 5_100, 5_200).is_none());
        calls.set_owner(5_000, 60, "SetTimezone", 1);
        assert_eq!(
            calls.explaining(1, 5_100, 5_200).map(|c| c.client_pid),
            Some(60)
        );
    }
}
