//! GRUB's environment block (`/boot/grub2/grubenv`): a 1024-byte file of `key=value` lines,
//! padded with `#`. Read here; written only by `grub2-editenv`, which keeps the block's size
//! and padding (GRUB itself rewrites the block in place at boot and refuses one that grew).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::BootError;

pub const HEADER: &str = "# GRUB Environment Block";

/// Parses a block. Values are unescaped (`\\` and `\` + newline, as grub2-editenv writes them).
///
/// # Errors
/// If the text doesn't start with GRUB's block header.
pub fn parse(text: &str) -> Result<BTreeMap<String, String>, BootError> {
    if !text.starts_with(HEADER) {
        return Err(BootError::Invalid(
            "grubenv has no GRUB environment block header".into(),
        ));
    }
    let mut vars = BTreeMap::new();
    let mut pending = String::new();
    for line in text.lines().skip(1) {
        if pending.is_empty() && line.starts_with('#') {
            continue;
        }
        pending.push_str(line);
        if pending.ends_with('\\') && !pending.ends_with("\\\\") {
            pending.pop();
            pending.push('\n');
            continue;
        }
        let entry = std::mem::take(&mut pending);
        if let Some((k, v)) = entry.split_once('=') {
            vars.insert(k.to_owned(), v.replace("\\\\", "\\"));
        }
    }
    Ok(vars)
}

///
/// # Errors
/// If the file can't be read or isn't a block.
pub fn read(path: &Path) -> Result<BTreeMap<String, String>, BootError> {
    let text = std::fs::read_to_string(path).map_err(|e| BootError::io(path, e))?;
    parse(&text)
}

/// `grub2-editenv <file> set|unset …`.
///
/// # Errors
/// If grub2-editenv can't be run or fails.
pub fn edit(editenv: &Path, file: &Path, args: &[&str]) -> Result<(), BootError> {
    let out = Command::new(editenv)
        .arg(file)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| BootError::io(editenv, e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(BootError::Tool(format!(
            "{} {} {}: {}",
            editenv.display(),
            file.display(),
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn parses_a_block() {
        let mut text = format!(
            "{HEADER}\nsaved_entry=48239e76-7.2.9\nboot_success=0\nkernelopts=root=UUID=x ro\n"
        );
        while text.len() < 1024 {
            text.push('#');
        }
        let vars = parse(&text).unwrap();
        assert_eq!(vars["saved_entry"], "48239e76-7.2.9");
        assert_eq!(vars["kernelopts"], "root=UUID=x ro");
        assert_eq!(vars.len(), 3);
    }

    #[test]
    fn unescapes() {
        let vars = parse(&format!("{HEADER}\na=one\\\ntwo\nb=back\\\\slash\n#####")).unwrap();
        assert_eq!(vars["a"], "one\ntwo");
        assert_eq!(vars["b"], "back\\slash");
    }

    #[test]
    fn refuses_a_non_block() {
        assert!(parse("saved_entry=x\n").is_err());
    }
}
