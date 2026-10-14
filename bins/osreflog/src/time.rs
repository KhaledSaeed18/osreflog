//! Local-time formatting without a time-zone database: the offset comes from `date +%z` once,
//! and dates from Howard Hinnant's `civil_from_days`.

use std::process::Command;
use std::sync::OnceLock;

fn offset_s() -> i64 {
    static OFFSET: OnceLock<i64> = OnceLock::new();
    *OFFSET.get_or_init(|| {
        let out = Command::new("date").arg("+%z").output().ok();
        let s = out
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default();
        parse_offset(&s).unwrap_or(0)
    })
}

/// `+0300` → 10800 seconds.
#[must_use]
pub fn parse_offset(s: &str) -> Option<i64> {
    let (sign, rest) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    if rest.len() != 4 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i64 = rest[..2].parse().ok()?;
    let m: i64 = rest[2..].parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `YYYY-MM-DD HH:MM:SS` for a Unix time in milliseconds, at `offset` seconds from UTC.
#[must_use]
pub fn format_at(ms: i64, offset: i64) -> String {
    let s = ms.div_euclid(1000) + offset;
    let (days, secs) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Local time for a Unix time in milliseconds.
#[must_use]
pub fn local(ms: i64) -> String {
    format_at(ms, offset_s())
}

/// Turns `@{<date expression>}` into `@{ms:<epoch ms>}` with `date -d`, leaving every form
/// the daemon understands alone.
#[must_use]
pub fn expand_ref(r: &str) -> String {
    let Some(inner) = r.strip_prefix("@{").and_then(|s| s.strip_suffix('}')) else {
        return r.to_owned();
    };
    let known = inner == "now"
        || inner == "boot"
        || inner.starts_with("ms:")
        || inner
            .strip_prefix("boot-")
            .is_some_and(|n| n.parse::<u32>().is_ok())
        || inner.parse::<u32>().is_ok();
    if known {
        return r.to_owned();
    }
    let out = Command::new("date").args(["-d", inner, "+%s%3N"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let ms = String::from_utf8_lossy(&o.stdout).trim().to_owned();
            if ms.parse::<i64>().is_ok() {
                format!("@{{ms:{ms}}}")
            } else {
                r.to_owned()
            }
        }
        _ => r.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_known_instants() {
        assert_eq!(format_at(0, 0), "1970-01-01 00:00:00");
        // 2026-10-15 00:35:26.071 +0300 (the P1 hook run).
        assert_eq!(format_at(1_792_013_726_071, 10_800), "2026-10-15 00:35:26");
        assert_eq!(format_at(951_782_400_000, 0), "2000-02-29 00:00:00");
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("+0300"), Some(10_800));
        assert_eq!(parse_offset("-0530"), Some(-19_800));
        assert_eq!(parse_offset("0300"), None);
        assert_eq!(parse_offset("+3"), None);
    }

    #[test]
    fn known_refs_pass_through() {
        for r in ["c42", "@{0}", "@{now}", "@{boot}", "@{boot-2}", "@{ms:5}"] {
            assert_eq!(expand_ref(r), r);
        }
        assert!(expand_ref("@{1970-01-02 00:00 UTC}").starts_with("@{ms:"));
    }
}
