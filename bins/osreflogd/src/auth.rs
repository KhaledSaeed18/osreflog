//! Who may call what. Root may call everything (the dnf hook runs as root); everyone else
//! goes through polkit: reading needs an active local session, a manual checkpoint needs
//! admin authentication (interactive, kept for a while). Adapted from drainscope's access
//! crate.

use std::collections::HashMap;

use zbus::Variant;
use zbus::names::BusName;

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn check_authorization(
        &self,
        subject: &(&str, HashMap<&str, Variant<'_>>),
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<(bool, bool, HashMap<String, String>)>;
}

const ALLOW_USER_INTERACTION: u32 = 1;

/// The Unix user behind a unique bus name.
///
/// # Errors
/// If the bus can't tell (e.g. the caller disconnected).
pub async fn caller_uid(connection: &zbus::Connection, caller: &str) -> zbus::Result<u32> {
    let name = BusName::try_from(caller)?;
    let uid = zbus::fdo::DBusProxy::new(connection)
        .await?
        .get_connection_unix_user(name)
        .await?;
    Ok(uid)
}

/// Asks polkit whether `caller` may perform `action`.
///
/// # Errors
/// If polkit can't be asked.
pub async fn polkit_allows(
    connection: &zbus::Connection,
    caller: &str,
    action: &str,
    interactive: bool,
) -> zbus::Result<bool> {
    let authority = AuthorityProxy::new(connection).await?;
    let subject = (
        "system-bus-name",
        HashMap::from([("name", Variant::from(caller))]),
    );
    let flags = if interactive {
        ALLOW_USER_INTERACTION
    } else {
        0
    };
    let (authorized, _challenge, _details) = authority
        .check_authorization(&subject, action, HashMap::new(), flags, "")
        .await?;
    Ok(authorized)
}
