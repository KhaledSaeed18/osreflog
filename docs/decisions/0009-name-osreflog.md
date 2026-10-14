# 0009: Name: osreflog

- Status: Accepted
- Date: 2026-10-14

## Context
The working name "rewind" was taken on every registry. There's also a US trademark REWIND (Class 9) for backup/restore software and a Linux "RewindOS" undo app, which makes "osrewind" risky too. About 40 names were screened ([naming report](../naming/name-check-round1.md)).

## Decision
**osreflog**: "git's reflog, for your OS". It was free on crates.io, GitHub, .com/.dev/.io, Fedora, AUR, Debian and Homebrew, with zero GitHub repos and no trademark hits, as of 2026-10-14.

## Alternatives considered
- **whychanged:** free and self-explaining, but reads as a phrase and is a weak brand.
- **osrewind:** friendlier, but carries the trademark and RewindOS risks.

## Consequences
- The git vocabulary becomes a design principle (`log`, `diff`, `blame`, `bisect`, `@{…}` refs).
- The name is opaque to people who don't use git; the tagline has to carry the meaning.
