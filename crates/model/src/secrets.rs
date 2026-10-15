//! What osreflog treats as secret: never stored as content, never restored by undo.

/// Paths whose content is never stored, whatever their mode.
const SECRET_PREFIXES: &[&str] = &[
    "/etc/shadow",
    "/etc/gshadow",
    "/etc/security/opasswd",
    "/etc/crypttab",
    "/etc/NetworkManager/system-connections/",
    "/etc/wireguard/",
    "/etc/ssh/ssh_host_",
    "/etc/pki/tls/private/",
    "/etc/sssd/",
    "/etc/krb5.keytab",
    "/etc/libvirt/secrets/",
];
const SECRET_SUFFIXES: &[&str] = &[".key", ".keytab", ".p12", ".pfx", "secret_key"];

/// Whether `path` names a secret by its location alone. Paths are case-sensitive.
#[must_use]
#[allow(clippy::case_sensitive_file_extension_comparisons)]
pub fn is_secret_path(path: &str) -> bool {
    if path.starts_with("/etc/ssh/ssh_host_") && path.ends_with(".pub") {
        return false;
    }
    SECRET_PREFIXES.iter().any(|p| path.starts_with(p))
        || SECRET_SUFFIXES.iter().any(|s| path.ends_with(s))
}

/// Whether file metadata marks it as secret: owned by root and unreadable by anyone else.
#[must_use]
#[allow(clippy::verbose_bit_mask)]
pub fn is_secret_mode(uid: u32, mode: u32) -> bool {
    uid == 0 && (mode & 0o077) == 0
}
