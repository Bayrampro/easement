use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// Lines added or changed since the merge-base with a base revision, including
/// uncommitted and untracked files. Paths are relative to the project root.
#[derive(Debug, Default)]
pub struct Changes {
    pub base: String,
    pub merge_base: String,
    files: HashMap<String, Lines>,
    /// Files the change deletes.
    pub deleted: Vec<String>,
}

#[derive(Debug)]
enum Lines {
    All,
    Ranges(Vec<(usize, usize)>),
}

impl Changes {
    pub fn collect(root: &Path, base: &str) -> Result<Self> {
        let merge_base = git(root, &["merge-base", base, "HEAD"]).with_context(|| {
            format!(
                "cannot find the merge-base of `{base}` and HEAD; \
                 in CI fetch the base branch too (actions/checkout: fetch-depth: 0)"
            )
        })?;
        let merge_base = merge_base.trim().to_string();
        // Working tree against the merge-base: committed and uncommitted changes.
        // --relative limits the diff to the project root and makes paths relative to it.
        let diff = git(
            root,
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                "--relative",
                "--unified=0",
                "--find-renames",
                "--no-color",
                "--no-ext-diff",
                "--no-prefix",
                &merge_base,
            ],
        )?;
        let mut changes = Self::parse(&diff);
        let untracked = git(
            root,
            &[
                "-c",
                "core.quotePath=false",
                "ls-files",
                "--others",
                "--exclude-standard",
            ],
        )?;
        for path in untracked.lines().filter(|l| !l.is_empty()) {
            changes.files.insert(path.to_string(), Lines::All);
        }
        changes.base = base.to_string();
        changes.merge_base = merge_base;
        Ok(changes)
    }

    fn parse(diff: &str) -> Self {
        let mut files: HashMap<String, Lines> = HashMap::new();
        let mut deleted = Vec::new();
        let mut old: Option<String> = None;
        let mut current: Option<String> = None;
        for line in diff.lines() {
            if let Some(path) = line.strip_prefix("--- ") {
                old = (path != "/dev/null").then(|| path.trim_matches('"').to_string());
            } else if let Some(path) = line.strip_prefix("+++ ") {
                current = (path != "/dev/null").then(|| path.trim_matches('"').to_string());
                if current.is_none() {
                    deleted.extend(old.take());
                }
            } else if line.starts_with("@@")
                && let (Some(path), Some(range)) = (&current, added_range(line))
                && let Lines::Ranges(ranges) = files
                    .entry(path.clone())
                    .or_insert_with(|| Lines::Ranges(Vec::new()))
            {
                ranges.push(range);
            }
        }
        Self {
            files,
            deleted,
            ..Self::default()
        }
    }

    /// Every file the change touches.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn contains(&self, file: &str, line: usize) -> bool {
        match self.files.get(file) {
            Some(Lines::All) => true,
            Some(Lines::Ranges(ranges)) => ranges.iter().any(|(a, b)| (*a..=*b).contains(&line)),
            None => false,
        }
    }
}

/// The added side of a hunk header `@@ -a,b +c,d @@`; `None` for pure deletions.
fn added_range(header: &str) -> Option<(usize, usize)> {
    let added = header.split_whitespace().find(|p| p.starts_with('+'))?;
    let mut parts = added[1..].splitn(2, ',');
    let start: usize = parts.next()?.parse().ok()?;
    let count: usize = match parts.next() {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    (count > 0).then(|| (start, start + count - 1))
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .context("cannot run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_deleted_files() {
        let diff = "\
--- src/gone.ts
+++ /dev/null
@@ -1,2 +0,0 @@
--- src/kept.ts
+++ src/kept.ts
@@ -1 +1,2 @@
";
        let changes = Changes::parse(diff);
        assert_eq!(changes.deleted, ["src/gone.ts"]);
        assert!(changes.contains("src/kept.ts", 1));
    }

    #[test]
    fn parses_hunks() {
        let diff = "\
diff --git src/a.ts src/a.ts
--- src/a.ts
+++ src/a.ts
@@ -3 +3 @@ x
-old
+new
@@ -10,0 +11,3 @@
+a
+b
+c
@@ -20,2 +23,0 @@
-gone
diff --git src/old.ts src/old.ts
--- src/old.ts
+++ /dev/null
@@ -1 +0,0 @@
-x
";
        let changes = Changes::parse(diff);
        assert!(changes.contains("src/a.ts", 3));
        assert!(!changes.contains("src/a.ts", 4));
        assert!(changes.contains("src/a.ts", 11));
        assert!(changes.contains("src/a.ts", 13));
        assert!(!changes.contains("src/a.ts", 14));
        assert!(!changes.contains("src/a.ts", 23));
        assert!(!changes.contains("src/old.ts", 1));
    }
}
