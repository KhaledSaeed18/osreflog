# Name check: rewind, osrewind, osreflog, whychanged (+ 40 screened)

**Recommendation:** **osreflog**. It is free on every surface checked (crates.io, GitHub handle, .com/.dev/.io/.sh, Fedora, AUR, Debian, Homebrew), has zero GitHub repos and no web or trademark hits. "reflog for your OS" names exactly what the tool does for its engineering audience. **The real risk:** it's insider vocabulary. People who don't know git's `reflog` won't get the name, and it's harder to say out loud than a plain word.
**Checked:** 2026-10-14 · A Linux system change-history tool (Rust daemon + CLI, later a GNOME app). It records what changed on the OS and why, using btrfs snapshots, and bisects regressions. Ships via GitHub (`KhaledSaeed18/<name>`), Fedora COPR, possibly crates.io and Flathub.

## Verdict at a glance

| Candidate | crates.io | GitHub handle | .com | .dev | .io / .sh | Fedora / AUR / Debian | GitHub repo collisions | Trademark signal | Verdict |
| --------- | --------- | ------------- | ---- | ---- | --------- | --------------------- | ---------------------- | ---------------- | ------- |
| rewind | taken | taken | taken | taken | – | free | 3,968 repos | **REWIND (Class 9, backup/restore software)** | blocked |
| osrewind | free | free | free | free | free / free | free | 0 exact; **RewindOS** (Linux file-undo app) | REWIND mark is a near match | risky |
| **osreflog** | free | free | free | free | free / unverified* | free | 0 | none found | **clear** |
| whychanged | free | free | free | free | free / unverified* | free | 0 | none found | clear, weak brand |

\* whois/RDAP not run for that TLD in this round.

## rewind (the working name)

| Surface | Checked via | Result | Verdict |
| ------- | ----------- | ------ | ------- |
| crates.io | `crates.io/api/v1/crates/rewind` | 200 | taken |
| GitHub handle | `api.github.com/users/rewind` | 200 | taken |
| .com / .dev | RDAP | 200 / 200 | taken |
| GitHub repos | `gh api search/repositories q="rewind in:name"` | 3,968 repos; gilesbowkett/rewind ★1.8k | crowded |

**Trademark signals:** a US word mark **REWIND**, Reg. 4195323, Class 9, "computer software for archiving, backing up and restoring data", registered 2012 ([GleanMark listing](https://gleanmark.com/owner/rewindsoftwareinc)). Rewind Software (rewind.com, Ottawa) sells SaaS backups, including for GitHub ([AWS profile](https://aws.amazon.com/startups/learn/turning-back-the-data-clock-with-rewind), [review](https://ecommerce-platforms.com/online-backup-services/rewind-review)). That's the same goods description as this tool. Not legal advice; I haven't verified the mark's owner or status on USPTO directly.
**Verdict:** dead on availability, findability and trademark.

## osrewind

| Surface | Checked via | Result | Verdict |
| ------- | ----------- | ------ | ------- |
| crates.io | `crates.io/api/v1/crates/osrewind` (+ `os-rewind`, `rewindos`) | 404 ×3 | free |
| GitHub handle | `api.github.com/users/osrewind` | 404 | free |
| .com | `whois osrewind.com` | No match | free |
| .dev | RDAP | 404 | free |
| .io / .sh | `whois` | Domain not found | free |
| .app / .org | RDAP | 404 / 404 | free |
| Fedora / AUR / Debian / Homebrew | src.fedoraproject.org API / AUR RPC / sources.debian.org API / formulae.brew.sh | 404 / 0 / not found / 404 | free |
| Shell command | `command -v osrewind` | none | free |
| Flathub | search API | fuzzy hits only, no exact app | free |

**Collisions:** [rahmiapps/RewindOS](https://github.com/rahmiapps/RewindOS) (★1) is a "local undo and recovery center for Windows and Linux" that tracks file changes and restores them. That's the same problem space with a swapped name. It's tiny, but it's a direct near miss. [Mathieu-Poirier/RewindOS](https://github.com/Mathieu-Poirier/RewindOS) is an embedded OS about snapshot recovery. Linux Mint's **Timeshift** owns the "time travel for your system" metaphor.
**Findability:** a search for `"osrewind"` returned only generic "rewind" pages. Engines split it into "os rewind" and drown it in the common word.
**Trademark signals:** it contains the REWIND Class 9 mark above, for the same kind of software.
**Risks:** trademark proximity, plus the RewindOS near miss in the exact same niche.

## osreflog

| Surface | Checked via | Result | Verdict |
| ------- | ----------- | ------ | ------- |
| crates.io | `crates.io/api/v1/crates/osreflog` (+ `os-reflog`, `reflogd`) | 404 ×3 | free |
| GitHub handle | `api.github.com/users/osreflog` | 404 | free |
| .com | `whois osreflog.com` | No match | free |
| .dev | Google Registry RDAP | 404 | free |
| .io | `whois osreflog.io` | Domain not found | free |
| .sh / .app / .org | rdap.org | 429 rate-limited | unverified |
| Fedora / AUR / Debian / Homebrew | same APIs as above | 404 / 0 / not found / 404 | free |
| Shell command | `command -v osreflog` | none | free |
| GitHub repos | `gh api search/repositories` | 0 | free |
| Flathub | search API | fuzzy hits only (log viewers) | free |

**Collisions:** none. The closest is openSUSE YaST's old `OSRLogFile` module ([docs](https://doc.opensuse.org/projects/YaST/SLES11/modules/OSRLogFile.html)), which is unrelated and dormant.
**Findability:** zero competing results for the exact term, so the project would own it from day one. The catch is that nobody searches for this word until they've heard of the project.
**Trademark signals:** none found in web search; USPTO/EUIPO not searched directly (see below). Not legal advice.
**Risks:** obscure to people who don't use git, and awkward out loud ("O-S ref-log").

## whychanged

Free on every surface checked: crates.io 404, GitHub handle 404, .com no match, .dev 404, .io not found, Fedora/AUR/Debian/Homebrew free, 0 GitHub repos, no web hits. As a command it reads like a sentence (`whychanged /etc/resolv.conf`) and leads with the tool's most original feature: recording *why* something changed. **Risks:** it reads as a phrase rather than a name, makes a weak logo and wordmark, and search engines treat it as the two common words "why changed".

## Screened out (registry sweep, 2026-10-14)

| Name | Why it's out |
| ---- | ------------ |
| culprit, backtrack, retrace, timewarp, hindsight, tidemark, tideline, spoor, mneme, rearview, strata, anamnesis, alibi | crates.io name taken (hindsight also a Debian package) |
| lastgood | [MalyStern/lastgood](https://github.com/MalyStern/lastgood) is a "it worked yesterday… tells you what changed" regression tool, plus an active `lastgood-telemetry` org |
| knowngood | `knowngood.sh` registered 2026-08-20 for an active dev tool (knowngood-sh org) |
| unbreak | 596 GitHub repos; .com/.app taken |
| sysbisect, osbisect, statebisect, bisectd | free, but only name one feature |
| osblame, sysblame, rootblame, driftblame, snapblame | free, but "blame" is a negative brand |
| osalibi, sysalibi, rootalibi | free, but an alibi proves someone *didn't* do it, which is the opposite meaning |
| sysrewind | 2 small `SysRewind` repos; .com taken |
| whatchanged, whodunit, whodidit, driftlog, lookback, deadreckon, ichnos, provd, annals, tracemark, sysdelta, changelens, changetrail, roottrail, statetrail, osledger, rootledger, syswitness, oswitness, sysreflog, sysculprit, whybroke | at least one of: GitHub handle or .com taken, weaker meaning, or dominated by a finalist |

## Not checked

- **USPTO / EUIPO / WIPO direct search:** these need an interactive browser. The REWIND mark came from a third-party aggregator; confirm it on [tmsearch.uspto.gov](https://tmsearch.uspto.gov) before relying on it either way. Nothing here is legal advice or a clearance search.
- **Social handles (X, Reddit):** skipped; this is an open-source tool, not a consumer brand.
- **npm, PyPI, app stores, Docker Hub:** n/a; it doesn't ship there.
- **.sh/.app/.org for osreflog and whychanged:** rdap.org rate-limited (429). Note that rdap.org returned **404 for `knowngood.sh` even though whois shows it registered**, so treat rdap.org results for `.sh` as unreliable and use `whois`.

## If you take osreflog

1. Create the GitHub repo `KhaledSaeed18/osreflog`. Optionally grab the `osreflog` org handle so nobody squats it.
2. Publish a placeholder crate on crates.io once there's a real `Cargo.toml` (crates.io names can't be reserved without publishing).
3. Register `osreflog.dev` only if you want a docs site; it's cheap and nobody is competing for it.
4. Create the COPR project `khaledsaeed18/osreflog` when the first package is ready, as you did for drainscope.
