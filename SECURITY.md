# Security

osreflogd runs as root, reads every file on the system it watches, and, when someone asks it
to undo a change, writes to it. A bug in it can matter, so please report one privately first.

## Reporting

Use GitHub's private reporting: **[Report a vulnerability](https://github.com/KhaledSaeed18/osreflog/security/advisories/new)**.
Say what you found, how to reproduce it, and what it lets an attacker do. You'll get an answer
within a week; a fix and an advisory follow, crediting you unless you'd rather not.

Please don't open a public issue for a vulnerability.

## What counts

Anything that breaks a promise in the [threat model](docs/threat-model.md), for example:

- undo writing outside the path it restores, or overwriting a change made after the cause
  (symlink or rename races, `..`, magic links, bind mounts);
- a secret's content reaching the ledger, a blob, the attic, logs or the D-Bus API;
- a local user reading the history, or taking a checkpoint, undoing or bisecting, without the
  polkit authorization those need;
- a probe changing the real system, the real ledger, or the default boot entry;
- input (a send stream, an rpm header, a config file, a D-Bus call) crashing or hanging the
  daemon, or making it run code.

Already known and documented, so not a report on its own: osreflogd runs as root with a
SELinux type of `unconfined_service_t`; anyone at the active local session can read the history
(that's the polkit default, and it can be tightened with a polkit rule).

## Supported versions

Fixes go into the latest release, in COPR and on the releases page.
