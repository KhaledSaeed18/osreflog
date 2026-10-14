//! rpm's version comparison (`rpmvercmp`), ported from rpm's `rpmio/rpmvercmp.c`, and EVR
//! ordering on top of it.

use std::cmp::Ordering;

/// Compares two version (or release) strings the way rpm does.
#[must_use]
pub fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    let sep = |c: u8| !c.is_ascii_alphanumeric() && c != b'~' && c != b'^';
    while i < a.len() || j < b.len() {
        while i < a.len() && sep(a[i]) {
            i += 1;
        }
        while j < b.len() && sep(b[j]) {
            j += 1;
        }
        let (ca, cb) = (a.get(i).copied(), b.get(j).copied());
        // Tilde sorts before everything, even the end of the string.
        if ca == Some(b'~') || cb == Some(b'~') {
            if ca != Some(b'~') {
                return Ordering::Greater;
            }
            if cb != Some(b'~') {
                return Ordering::Less;
            }
            i += 1;
            j += 1;
            continue;
        }
        // Caret sorts after the end of the string but before anything else.
        if ca == Some(b'^') || cb == Some(b'^') {
            if ca.is_none() {
                return Ordering::Less;
            }
            if cb.is_none() {
                return Ordering::Greater;
            }
            if ca != Some(b'^') {
                return Ordering::Greater;
            }
            if cb != Some(b'^') {
                return Ordering::Less;
            }
            i += 1;
            j += 1;
            continue;
        }
        if ca.is_none() || cb.is_none() {
            break;
        }
        let numeric = a[i].is_ascii_digit();
        let class = |c: &u8| {
            if numeric {
                c.is_ascii_digit()
            } else {
                c.is_ascii_alphabetic()
            }
        };
        let ea = i + a[i..].iter().take_while(|c| class(c)).count();
        let eb = j + b[j..].iter().take_while(|c| class(c)).count();
        let (mut sa, mut sb) = (&a[i..ea], &b[j..eb]);
        if sb.is_empty() {
            // Segments of different types: numeric is newer than alpha.
            return if numeric {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        if numeric {
            while sa.first() == Some(&b'0') {
                sa = &sa[1..];
            }
            while sb.first() == Some(&b'0') {
                sb = &sb[1..];
            }
            match sa.len().cmp(&sb.len()) {
                Ordering::Equal => {}
                other => return other,
            }
        }
        match sa.cmp(sb) {
            Ordering::Equal => {}
            other => return other,
        }
        i = ea;
        j = eb;
    }
    match (i >= a.len(), j >= b.len()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        _ => Ordering::Greater,
    }
}

/// Compares epoch (missing = 0), then version, then release.
#[must_use]
pub fn evrcmp(a: (Option<u32>, &str, &str), b: (Option<u32>, &str, &str)) -> Ordering {
    a.0.unwrap_or(0)
        .cmp(&b.0.unwrap_or(0))
        .then_with(|| rpmvercmp(a.1, b.1))
        .then_with(|| rpmvercmp(a.2, b.2))
}

#[cfg(test)]
mod tests {
    use super::rpmvercmp;
    use std::cmp::Ordering::{Equal, Greater, Less};

    // Cases from rpm's tests/rpmvercmp.at.
    #[test]
    fn rpm_test_suite() {
        let cases = [
            ("1.0", "1.0", Equal),
            ("1.0", "2.0", Less),
            ("2.0", "1.0", Greater),
            ("2.0.1", "2.0.1", Equal),
            ("2.0", "2.0.1", Less),
            ("2.0.1a", "2.0.1", Greater),
            ("5.5p1", "5.5p2", Less),
            ("5.5p10", "5.5p1", Greater),
            ("10xyz", "10.1xyz", Less),
            ("xyz10", "xyz10.1", Less),
            ("xyz.4", "8", Less),
            ("8", "xyz.4", Greater),
            ("1.0010", "1.9", Greater),
            ("1.05", "1.5", Equal),
            ("1.0", "1", Greater),
            ("2.50", "2.5", Greater),
            ("fc4", "fc.4", Equal),
            ("FC5", "fc4", Less),
            ("2a", "2.0", Less),
            ("1.0a", "1.0", Greater),
            ("1.0~rc1", "1.0", Less),
            ("1.0~rc1", "1.0~rc2", Less),
            ("1.0~rc1~git123", "1.0~rc1", Less),
            ("1.0^", "1.0", Greater),
            ("1.0^git1", "1.0", Greater),
            ("1.0^git1", "1.0^git2", Less),
            ("1.0^git1", "1.01", Less),
            ("1.0^20160101", "1.0.1", Less),
            ("1.0~rc1^git1", "1.0~rc1", Greater),
            ("1.0^git1~pre", "1.0^git1", Less),
        ];
        for (a, b, want) in cases {
            assert_eq!(rpmvercmp(a, b), want, "{a} vs {b}");
        }
    }
}
