//! A project file is named by a `/`-separated path relative to the project root.

use std::path::Path;

/// The extension of the file, `""` without one: `ts` of `src/a.d.ts`.
pub fn extension(path: &str) -> &str {
    Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
}

/// The directory of the file, `""` at the root.
pub fn parent(path: &str) -> &str {
    Path::new(path)
        .parent()
        .and_then(|dir| dir.to_str())
        .unwrap_or("")
}

/// The name of the file, the last part of its path.
pub fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_project_path() {
        assert_eq!(extension("src/a.d.ts"), "ts");
        assert_eq!(extension("Makefile"), "");
        assert_eq!(extension("v1.2/Makefile"), "");
        assert_eq!(parent("src/web/a.ts"), "src/web");
        assert_eq!(parent("a.ts"), "");
        assert_eq!(file_name("src/web/a.ts"), "a.ts");
        assert_eq!(file_name("a.ts"), "a.ts");
    }
}
