//! A small line diff (Myers' O(ND) algorithm) rendered as unified-diff hunks, for showing what
//! changed inside a config file.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Keep,
    Del,
    Add,
}

/// Past this many edits the middle is shown as replaced outright: a diff that large isn't
/// readable line by line, and the search's memory grows with the square of it.
pub const MAX_EDITS: usize = 1000;

/// The shortest edit script turning `a` into `b`, as (edit, line); for inputs more than
/// [`MAX_EDITS`] apart, a correct but not minimal one (common ends kept, the middle replaced).
#[must_use]
pub fn diff_lines<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(Edit, &'a str)> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (ma, mb) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut out: Vec<(Edit, &'a str)> = a[..prefix].iter().map(|l| (Edit::Keep, *l)).collect();
    if let Some(middle) = myers(ma, mb) {
        out.extend(middle);
    } else {
        out.extend(ma.iter().map(|l| (Edit::Del, *l)));
        out.extend(mb.iter().map(|l| (Edit::Add, *l)));
    }
    out.extend(a[a.len() - suffix..].iter().map(|l| (Edit::Keep, *l)));
    out
}

/// Myers' search, or `None` past [`MAX_EDITS`].
///
/// Myers works in diagonal coordinates `k = x - y`, which go negative; inputs are bounded
/// (files of at most 1 MiB), so the index casts below can't wrap. Step `d` reads only the
/// diagonals `-(d+1)..=d+1`, so that window is all the trace keeps for backtracking.
// n, m, d, k, v, x, y: the paper's names.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::many_single_char_names
)]
fn myers<'a>(a: &[&'a str], b: &[&'a str]) -> Option<Vec<(Edit, &'a str)>> {
    let (n, m) = (a.len(), b.len());
    let max = (n + m).min(MAX_EDITS);
    let offset = max as isize + 1;
    let idx = |k: isize| (k + offset) as usize;
    let mut v = vec![0usize; 2 * max + 3];
    let mut trace: Vec<Vec<usize>> = Vec::new();
    let mut found = false;
    'outer: for d in 0..=max {
        let di = d as isize;
        trace.push(v[idx(-di - 1)..=idx(di + 1)].to_vec());
        for k in (-di..=di).step_by(2) {
            let mut x = if k == -di || (k != di && v[idx(k - 1)] < v[idx(k + 1)]) {
                v[idx(k + 1)]
            } else {
                v[idx(k - 1)] + 1
            };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            v[idx(k)] = x;
            if x >= n && y >= m {
                found = true;
                break 'outer;
            }
        }
    }
    if !found {
        return None;
    }
    // Backtrack through the saved windows; trace[d][j] is diagonal j - (d + 1).
    let mut out = Vec::new();
    let (mut x, mut y) = (n as isize, m as isize);
    for d in (0..trace.len()).rev() {
        let w = &trace[d];
        let d = d as isize;
        let at = |k: isize| w[(k + d + 1) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = at(prev_k) as isize;
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            out.push((Edit::Keep, a[(x - 1) as usize]));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == prev_x {
                out.push((Edit::Add, b[(y - 1) as usize]));
            } else {
                out.push((Edit::Del, a[(x - 1) as usize]));
            }
        }
        x = prev_x;
        y = prev_y;
    }
    out.reverse();
    Some(out)
}

/// Unified-diff hunks (no file headers) with `context` lines around each change.
#[must_use]
pub fn unified(before: &str, after: &str, context: usize) -> String {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let edits = diff_lines(&a, &b);
    let changed: Vec<usize> = edits
        .iter()
        .enumerate()
        .filter(|(_, (e, _))| *e != Edit::Keep)
        .map(|(i, _)| i)
        .collect();
    if changed.is_empty() {
        return String::new();
    }
    // Group changes whose context overlaps into hunks.
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &i in &changed {
        let start = i.saturating_sub(context);
        let end = (i + context + 1).min(edits.len());
        match hunks.last_mut() {
            Some(h) if start <= h.1 => h.1 = end,
            _ => hunks.push((start, end)),
        }
    }
    let mut out = String::new();
    // Line numbers before each hunk, counted in one pass over the edits.
    let (mut la, mut lb, mut seen) = (1, 1, 0);
    for (start, end) in hunks {
        for (e, _) in &edits[seen..start] {
            match e {
                Edit::Keep => {
                    la += 1;
                    lb += 1;
                }
                Edit::Del => la += 1,
                Edit::Add => lb += 1,
            }
        }
        seen = start;
        let count_a = edits[start..end]
            .iter()
            .filter(|(e, _)| *e != Edit::Add)
            .count();
        let count_b = edits[start..end]
            .iter()
            .filter(|(e, _)| *e != Edit::Del)
            .count();
        let _ = writeln!(out, "@@ -{la},{count_a} +{lb},{count_b} @@");
        for (e, line) in &edits[start..end] {
            let mark = match e {
                Edit::Keep => ' ',
                Edit::Del => '-',
                Edit::Add => '+',
            };
            out.push(mark);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_script_is_minimal_and_correct() {
        let a = ["a", "b", "c", "a", "b", "b", "a"];
        let b = ["c", "b", "a", "b", "a", "c"];
        let edits = diff_lines(&a, &b);
        let changes = edits.iter().filter(|(e, _)| *e != Edit::Keep).count();
        assert_eq!(changes, 5, "Myers' example has distance 5");
        let rebuilt: Vec<&str> = edits
            .iter()
            .filter(|(e, _)| *e != Edit::Del)
            .map(|(_, l)| *l)
            .collect();
        assert_eq!(rebuilt, b);
        let original: Vec<&str> = edits
            .iter()
            .filter(|(e, _)| *e != Edit::Add)
            .map(|(_, l)| *l)
            .collect();
        assert_eq!(original, a);
    }

    #[test]
    fn far_apart_inputs_stay_bounded_and_correct() {
        let a: Vec<String> = (0..20_000).map(|i| format!("a{i}")).collect();
        let b: Vec<String> = (0..20_000).map(|i| format!("b{i}")).collect();
        let (mut a, mut b): (Vec<&str>, Vec<&str>) = (
            a.iter().map(String::as_str).collect(),
            b.iter().map(String::as_str).collect(),
        );
        a.insert(0, "same head");
        b.insert(0, "same head");
        a.push("same tail");
        b.push("same tail");
        let edits = diff_lines(&a, &b);
        let side = |skip: Edit| -> Vec<&str> {
            edits
                .iter()
                .filter(|(e, _)| *e != skip)
                .map(|(_, l)| *l)
                .collect()
        };
        assert_eq!(side(Edit::Add), a);
        assert_eq!(side(Edit::Del), b);
        assert_eq!(edits.first(), Some(&(Edit::Keep, "same head")));
        assert_eq!(edits.last(), Some(&(Edit::Keep, "same tail")));
    }

    #[test]
    fn unified_hunks() {
        let before = "127.0.0.1 localhost\n::1 localhost\n";
        let after = "127.0.0.1 localhost\n::1 localhost\n10.0.0.2 nas\n";
        assert_eq!(
            unified(before, after, 3),
            "@@ -1,2 +1,3 @@\n 127.0.0.1 localhost\n ::1 localhost\n+10.0.0.2 nas\n"
        );
        assert_eq!(unified("x\n", "x\n", 3), "");
        let before = (1..=20).fold(String::new(), |mut s, i| {
            let _ = writeln!(s, "l{i}");
            s
        });
        let after = before.replace("l3\n", "L3\n").replace("l18\n", "L18\n");
        let u = unified(&before, &after, 1);
        assert_eq!(u.matches("@@").count(), 4, "two separate hunks");
        assert!(u.contains("-l3\n+L3\n") && u.contains("-l18\n+L18\n"));
        assert_eq!(unified("", "new\n", 3), "@@ -1,0 +1,1 @@\n+new\n");
    }
}
