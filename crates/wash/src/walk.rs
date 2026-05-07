//! File enumeration with `.gitignore` support, optionally narrowed by glob patterns.

use anyhow::Result;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub struct Walk {
    pub patterns: Vec<String>,
    pub root: PathBuf,
}

impl Walk {
    pub fn new(root: impl Into<PathBuf>, patterns: Vec<String>) -> Self {
        Self { root: root.into(), patterns }
    }

    /// Enumerate files under `root` that match any of the configured glob patterns.
    /// Honors `.gitignore` (via the `ignore` crate) plus the hardcoded skips matching
    /// the legacy JS implementation: `.git` and `node_modules`.
    pub fn files(&self) -> Result<Vec<PathBuf>> {
        let matches_all = self.patterns.is_empty()
            || self
                .patterns
                .iter()
                .any(|p| p == "**/*" || p == "**" || p.is_empty());

        let set = if matches_all {
            None
        } else {
            let mut b = GlobSetBuilder::new();
            for p in &self.patterns {
                let translated = translate_glob(p);
                if let Ok(g) = Glob::new(&translated) {
                    b.add(g);
                }
                // Also accept the literal pattern in case caller passed a globset-native form.
                if let Ok(g) = Glob::new(p) {
                    b.add(g);
                }
            }
            Some(b.build()?)
        };

        let mut out = Vec::new();
        for entry in WalkBuilder::new(&self.root)
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .require_git(false)
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                name != ".git" && name != "node_modules"
            })
            .build()
        {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let abs = entry.path().to_path_buf();
            if let Some(set) = &set {
                let rel = relativize(&self.root, &abs);
                if !set.is_match(&rel) {
                    continue;
                }
            }
            out.push(abs);
        }
        Ok(out)
    }
}

/// Convert a gitignore-style glob to globset syntax. Both share most semantics; we just
/// need to handle the leading `/` (which gitignore uses for "anchor at root") by stripping it.
fn translate_glob(p: &str) -> String {
    let mut g = p;
    if let Some(stripped) = g.strip_prefix('/') {
        g = stripped;
    }
    // If the glob has no slash and isn't already broadened, broaden it so it matches any path.
    if !g.contains('/') && !g.starts_with("**/") {
        return format!("**/{g}");
    }
    g.to_string()
}

pub fn relativize(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .unwrap_or(abs)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn skips_node_modules_and_git() {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        fs::create_dir_all(p.join("node_modules/foo")).unwrap();
        fs::write(p.join("node_modules/foo/x.js"), "x").unwrap();
        fs::create_dir_all(p.join(".git")).unwrap();
        fs::write(p.join(".git/HEAD"), "ref").unwrap();
        fs::write(p.join("a.ts"), "1").unwrap();
        let w = Walk::new(p, vec!["**/*".into()]);
        let files = w.files().unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|f| f.strip_prefix(p).unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.ts".to_string()));
        assert!(!names.iter().any(|n| n.contains("node_modules")));
        assert!(!names.iter().any(|n| n.starts_with(".git")));
    }

    #[test]
    fn glob_filters_extension() {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        fs::write(p.join("a.ts"), "1").unwrap();
        fs::write(p.join("b.txt"), "1").unwrap();
        let w = Walk::new(p, vec!["**/*.ts".into()]);
        let files = w.files().unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.ts".to_string()));
        assert!(!names.contains(&"b.txt".to_string()));
    }
}
