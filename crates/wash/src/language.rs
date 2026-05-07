use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
    Python,
    Go,
    Rust,
    Unknown,
}

impl Language {
    pub fn detect(path: &str) -> Self {
        let ext = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        match ext {
            "ts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "js" | "mjs" | "cjs" => Language::JavaScript,
            "jsx" => Language::Jsx,
            "py" => Language::Python,
            "go" => Language::Go,
            "rs" => Language::Rust,
            _ => Language::Unknown,
        }
    }

    /// Stable string identifier matching the JS `detectLanguage` return values.
    pub fn as_str(self) -> &'static str {
        match self {
            Language::TypeScript | Language::Tsx => "typescript",
            Language::JavaScript | Language::Jsx => "javascript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Rust => "rust",
            Language::Unknown => "unknown",
        }
    }

    /// Whether tree-sitter signature extraction is available for this language. The remaining
    /// languages fall back to line-regex extraction (matching the legacy JS behavior) until
    /// their grammar crates land in a follow-up PR.
    pub fn has_tree_sitter(self) -> bool {
        matches!(
            self,
            Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_extensions() {
        assert_eq!(Language::detect("a/b/c.ts"), Language::TypeScript);
        assert_eq!(Language::detect("a/b/c.tsx"), Language::Tsx);
        assert_eq!(Language::detect("a/b/c.js"), Language::JavaScript);
        assert_eq!(Language::detect("a/b/c.mjs"), Language::JavaScript);
        assert_eq!(Language::detect("a/b/c.py"), Language::Python);
        assert_eq!(Language::detect("a/b/c.go"), Language::Go);
        assert_eq!(Language::detect("a/b/c.rs"), Language::Rust);
        assert_eq!(Language::detect("a/b/c.txt"), Language::Unknown);
        assert_eq!(Language::detect("noext"), Language::Unknown);
    }

    #[test]
    fn as_str_matches_legacy() {
        assert_eq!(Language::TypeScript.as_str(), "typescript");
        assert_eq!(Language::Tsx.as_str(), "typescript");
        assert_eq!(Language::JavaScript.as_str(), "javascript");
        assert_eq!(Language::Unknown.as_str(), "unknown");
    }
}
