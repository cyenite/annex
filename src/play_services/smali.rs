//! Small edits to apktool's smali output. Each finds its method by its exact declaration and
//! fails rather than guess when the code doesn't look as expected.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The label a guard jumps to when it lets the method carry on.
pub const CONTINUE: &str = ":annex_continue";

/// A method's lines from `.method` to `.end method`, and its `.locals`.
pub struct Method {
    pub end: usize,
    pub start: usize,
    pub locals_line: usize,
    pub locals: u32,
}

pub fn method(lines: &[&str], name: &str, is_declaration: impl Fn(&str) -> bool) -> Result<Method> {
    let starts: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with(".method ") && is_declaration(lines[i]))
        .collect();
    let [start] = starts[..] else {
        bail!("expected one {name}, found {}", starts.len());
    };
    let end = (start..lines.len())
        .find(|&i| lines[i] == ".end method")
        .with_context(|| format!("{name} has no end"))?;
    let locals_line = (start..end)
        .find(|&i| lines[i].trim_start().starts_with(".locals "))
        .with_context(|| format!("{name} has no .locals"))?;
    let locals = lines[locals_line]
        .trim()
        .trim_start_matches(".locals ")
        .parse()
        .with_context(|| format!("{name} has a malformed .locals"))?;
    Ok(Method {
        end,
        start,
        locals_line,
        locals,
    })
}

/// The line of a method's first instruction, after the annotations and parameter names that
/// follow `.locals`.
pub fn first_instruction(lines: &[&str], method: &Method) -> Result<usize> {
    let block_end = |from: usize, end: &str| {
        (from..method.end)
            .find(|&i| lines[i].trim() == end)
            .map(|i| i + 1)
            .with_context(|| format!("unclosed block before {end}"))
    };
    let mut i = method.locals_line + 1;
    while i < method.end {
        let line = lines[i].trim();
        if line.is_empty() {
            i += 1;
        } else if line.starts_with(".annotation ") {
            i = block_end(i, ".end annotation")?;
        } else if line.starts_with(".param ") {
            let next = (i + 1..method.end).find(|&j| !lines[j].trim().is_empty());
            let has_block = next.is_some_and(|j| lines[j].trim().starts_with(".annotation "));
            i = if has_block {
                block_end(i, ".end param")?
            } else {
                i + 1
            };
        } else {
            return Ok(i);
        }
    }
    bail!("the method has no instructions")
}

/// Runs `guard` at the start of the method declared exactly as `declaration`. The guard gets a
/// register of its own, added to the method's locals, and jumps to `CONTINUE` to carry on.
pub fn guard_entry(
    smali: &str,
    declaration: &str,
    guard: impl Fn(&str) -> Vec<String>,
) -> Result<String> {
    let lines: Vec<&str> = smali.lines().collect();
    let method = method(&lines, declaration, |line| line == declaration)?;
    if lines[method.start..method.end]
        .iter()
        .any(|line| line.trim() == CONTINUE)
    {
        bail!("{declaration} is already guarded");
    }
    let entry = first_instruction(&lines, &method)?;
    let register = format!("v{}", method.locals);
    let mut guarded = Vec::with_capacity(lines.len() + 12);
    for (i, line) in lines.iter().enumerate() {
        if i == method.locals_line {
            guarded.push(format!("    .locals {}", method.locals + 1));
            continue;
        }
        if i == entry {
            for instruction in guard(&register) {
                guarded.push(format!("    {instruction}"));
                guarded.push(String::new());
            }
            guarded.push(format!("    {CONTINUE}"));
        }
        guarded.push(line.to_string());
    }
    Ok(finish(guarded, smali))
}

/// Joins edited lines back into a file, keeping the original's trailing newline.
pub fn finish(lines: Vec<String>, original: &str) -> String {
    let mut joined = lines.join("\n");
    if original.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// apktool's code folders in a decoded package, in dex order: `smali`, `smali_classes2`...
pub fn folders(decoded: &Path) -> Result<Vec<PathBuf>> {
    let mut folders: Vec<PathBuf> = fs::read_dir(decoded)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("smali"))
        })
        .collect();
    folders.sort_by_key(|folder| dex_number(folder));
    if folders.is_empty() {
        bail!("{} has no code", decoded.display());
    }
    Ok(folders)
}

/// The one file for `class` (like `com/example/Name.smali`) across every dex.
pub fn class(folders: &[PathBuf], class: &str) -> Result<PathBuf> {
    let found: Vec<PathBuf> = folders
        .iter()
        .map(|folder| folder.join(class))
        .filter(|path| path.is_file())
        .collect();
    match &found[..] {
        [one] => Ok(one.clone()),
        _ => bail!("expected one {class}, found {}", found.len()),
    }
}

/// Adds a class to the last dex, the smallest and the one least likely to be near the 64K
/// method limit.
pub fn add_class(folders: &[PathBuf], class: &str, smali: &str) -> Result<()> {
    let last = folders.last().context("the package has no code")?;
    let path = last.join(class);
    fs::create_dir_all(path.parent().expect("a class has a folder"))?;
    fs::write(path, smali)?;
    Ok(())
}

fn dex_number(folder: &Path) -> u32 {
    folder
        .file_name()
        .and_then(|name| name.to_str()?.strip_prefix("smali_classes")?.parse().ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECLARATION: &str = ".method public static check(Landroid/content/Context;)V";
    const CLASS: &str = "\
.class public LExample;

.method public static check(Landroid/content/Context;)V
    .locals 2
    .annotation system Ldalvik/annotation/Signature;
        value = {
            \"x\"
        }
    .end annotation

    .param p0, \"context\"
    .annotation build Landroidx/annotation/NonNull;
    .end annotation
    .end param

    .param p1, \"plain\"

    .line 3
    const/4 v0, 0x0

    return-void
.end method
";

    fn guard(register: &str) -> Vec<String> {
        vec![
            format!("const/4 {register}, 0x1"),
            format!("if-eqz {register}, {CONTINUE}"),
            "return-void".to_string(),
        ]
    }

    #[test]
    fn guards_after_annotations_and_parameters() {
        let guarded = guard_entry(CLASS, DECLARATION, guard).unwrap();
        assert!(guarded.contains("    .locals 3\n    .annotation system"));
        assert!(guarded.contains(
            "    .param p1, \"plain\"\n\n    const/4 v2, 0x1\n\n    if-eqz v2, :annex_continue\n\n    return-void\n\n    :annex_continue\n    .line 3\n"
        ));
        assert!(guarded.ends_with(".end method\n"));
    }

    #[test]
    fn refuses_to_guard_twice_or_guess() {
        let guarded = guard_entry(CLASS, DECLARATION, guard).unwrap();
        assert!(guard_entry(&guarded, DECLARATION, guard).is_err());
        let other = DECLARATION.replace("check", "verify");
        assert!(guard_entry(CLASS, &other, guard).is_err());
        let registers = CLASS.replace(".locals 2", ".registers 3");
        assert!(guard_entry(&registers, DECLARATION, guard).is_err());
    }

    #[test]
    fn orders_dex_folders_by_number() {
        let mut folders = ["smali_classes10", "smali_classes2", "smali"].map(PathBuf::from);
        folders.sort_by_key(|folder| dex_number(folder));
        assert_eq!(
            folders.map(|folder| folder.display().to_string()),
            ["smali", "smali_classes2", "smali_classes10"]
        );
    }
}
