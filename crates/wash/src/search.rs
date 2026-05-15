//! Search engine: ripgrep-grade walking + matching, pure Rust (no shell-out to `rg`).
//! Returns ranked snippets across matched files.

use anyhow::Result;
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::walk::{Walk, relativize};

/// Default cap on per-file size for content scanning. Files larger than this are
/// skipped and reported in `SearchOutput::skipped` so the caller can react.
pub const DEFAULT_MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub path: String,
    #[serde(rename = "lineStart")]
    pub line_start: u32,
    #[serde(rename = "lineEnd")]
    pub line_end: u32,
    pub snippet: String,
    #[serde(rename = "matchCount")]
    pub match_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedFile {
    pub path: String,
    /// "size" — file exceeded `max_file_bytes`. "binary" — NUL byte detected during scan.
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchOutput {
    pub hits: Vec<SearchHit>,
    pub skipped: Vec<SkippedFile>,
}

pub struct SearchOpts {
    pub cwd: PathBuf,
    pub pattern: Option<String>,
    pub paths: Vec<String>,
    pub context_lines: u32,
    /// Per-file size cap; files over the limit are reported in `skipped`. `None` disables.
    pub max_file_bytes: Option<u64>,
}

pub fn run(opts: SearchOpts) -> Result<SearchOutput> {
    let walk = Walk::new(opts.cwd.clone(), opts.paths.clone());
    let files = walk.files()?;

    let Some(pattern) = opts.pattern else {
        let hits = files
            .iter()
            .map(|abs| SearchHit {
                path: relativize(&opts.cwd, abs),
                line_start: 0,
                line_end: 0,
                snippet: String::new(),
                match_count: 0,
            })
            .collect();
        return Ok(SearchOutput { hits, skipped: Vec::new() });
    };

    let matcher = RegexMatcher::new_line_matcher(&pattern)?;
    let mut searcher = SearcherBuilder::new()
        .before_context(opts.context_lines as usize)
        .after_context(opts.context_lines as usize)
        .line_number(true)
        // Stop scanning on the first NUL byte — matches ripgrep's default.
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();

    let mut hits = Vec::new();
    let mut skipped: Vec<SkippedFile> = Vec::new();
    for abs in &files {
        // Size guard: skip large files without opening them, before we hand them to the
        // searcher. A 500MB log doesn't get streamed line-by-line just to be discarded.
        if let Some(limit) = opts.max_file_bytes {
            if let Ok(meta) = std::fs::metadata(abs) {
                if meta.len() > limit {
                    skipped.push(SkippedFile {
                        path: relativize(&opts.cwd, abs),
                        reason: "size".into(),
                        bytes: Some(meta.len()),
                    });
                    continue;
                }
            }
        }

        let mut sink = HitSink::new();
        if searcher.search_path(&matcher, abs, &mut sink).is_err() {
            continue;
        }
        let is_binary = sink.is_binary;
        let snippets = sink.into_snippets(opts.context_lines);
        let rel = relativize(&opts.cwd, abs);
        if is_binary {
            skipped.push(SkippedFile { path: rel.clone(), reason: "binary".into(), bytes: None });
            continue;
        }
        if snippets.is_empty() {
            continue;
        }
        for snippet in snippets {
            hits.push(SearchHit {
                path: rel.clone(),
                line_start: snippet.line_start,
                line_end: snippet.line_end,
                snippet: snippet.text,
                match_count: snippet.match_count,
            });
        }
    }
    Ok(SearchOutput { hits, skipped })
}

struct HitSink {
    /// All lines we received from the searcher, keyed by line number (1-based).
    lines: BTreeMap<u32, String>,
    match_lines: HashSet<u32>,
    is_binary: bool,
}

struct GroupedSnippet {
    line_start: u32,
    line_end: u32,
    text: String,
    match_count: u32,
}

impl HitSink {
    fn new() -> Self {
        Self { lines: BTreeMap::new(), match_lines: HashSet::new(), is_binary: false }
    }

    fn record(&mut self, line: u32, bytes: &[u8], is_match: bool) {
        let s = std::str::from_utf8(bytes).unwrap_or("");
        let trimmed = s.strip_suffix('\n').unwrap_or(s).to_string();
        self.lines.insert(line, trimmed);
        if is_match {
            self.match_lines.insert(line);
        }
    }

    fn into_snippets(self, context_lines: u32) -> Vec<GroupedSnippet> {
        let mut snippets = Vec::new();
        let mut group: Vec<u32> = Vec::new();
        // BTreeMap iterates keys in sorted order — no extra sort needed.
        let iter: Vec<u32> = self.lines.keys().copied().collect();

        let flush = |group: &mut Vec<u32>, snippets: &mut Vec<GroupedSnippet>, lines: &BTreeMap<u32, String>, match_lines: &HashSet<u32>| {
            if group.is_empty() {
                return;
            }
            let start = *group.first().unwrap();
            let end = *group.last().unwrap();
            let snippet_lines: Vec<String> = group
                .iter()
                .map(|n| {
                    let body = lines.get(n).cloned().unwrap_or_default();
                    format!("{:>4}  {}", n, body)
                })
                .collect();
            let count = group.iter().filter(|n| match_lines.contains(n)).count() as u32;
            if count > 0 {
                snippets.push(GroupedSnippet {
                    line_start: start,
                    line_end: end,
                    text: snippet_lines.join("\n"),
                    match_count: count,
                });
            }
            group.clear();
        };

        for n in iter {
            if let Some(&last) = group.last() {
                if n - last <= context_lines + 1 {
                    group.push(n);
                    continue;
                }
                flush(&mut group, &mut snippets, &self.lines, &self.match_lines);
            }
            group.push(n);
        }
        flush(&mut group, &mut snippets, &self.lines, &self.match_lines);
        snippets
    }
}

impl Sink for HitSink {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, m: &SinkMatch) -> Result<bool, Self::Error> {
        if let Some(line) = m.line_number() {
            self.record(line as u32, m.bytes(), true);
        }
        Ok(true)
    }

    fn context(&mut self, _searcher: &Searcher, c: &SinkContext) -> Result<bool, Self::Error> {
        if let Some(line) = c.line_number() {
            self.record(line as u32, c.bytes(), false);
        }
        Ok(true)
    }

    fn binary_data(
        &mut self,
        _searcher: &Searcher,
        _binary_byte_offset: u64,
    ) -> Result<bool, Self::Error> {
        self.is_binary = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn finds_symbol_with_context() {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        fs::write(
            p.join("a.ts"),
            "export function foo() {\n  return 1;\n}\n\nexport function bar() {\n  return foo();\n}\n",
        )
        .unwrap();
        let out = run(SearchOpts {
            cwd: p.to_path_buf(),
            pattern: Some(r"\bfoo\b".into()),
            paths: vec!["**/*.ts".into()],
            context_lines: 1,
            max_file_bytes: Some(DEFAULT_MAX_FILE_BYTES),
        })
        .unwrap();
        assert!(!out.hits.is_empty(), "expected hits");
        // Found in two locations.
        let total_matches: u32 = out.hits.iter().map(|h| h.match_count).sum();
        assert!(total_matches >= 2, "got total_matches={total_matches}");
    }

    #[test]
    fn glob_only_no_pattern() {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        fs::write(p.join("a.ts"), "x").unwrap();
        fs::write(p.join("b.txt"), "x").unwrap();
        let out = run(SearchOpts {
            cwd: p.to_path_buf(),
            pattern: None,
            paths: vec!["**/*.ts".into()],
            context_lines: 0,
            max_file_bytes: Some(DEFAULT_MAX_FILE_BYTES),
        })
        .unwrap();
        let paths: Vec<&str> = out.hits.iter().map(|h| h.path.as_str()).collect();
        assert!(paths.contains(&"a.ts"));
        assert!(!paths.contains(&"b.txt"));
    }

    #[test]
    fn skips_binary_file_on_nul() {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        // File contains the pattern "needle" but also a NUL byte before it — ripgrep-style
        // binary detection should classify the file as binary and skip it.
        let mut bytes: Vec<u8> = b"prefix\x00 needle in binary\n".to_vec();
        // Add a second occurrence so a non-binary-aware searcher would still report a hit.
        bytes.extend_from_slice(b"needle line two\n");
        fs::write(p.join("blob.bin"), &bytes).unwrap();
        // Also add a clean text file with the same pattern to confirm normal hits still work.
        fs::write(p.join("ok.txt"), "needle in text\n").unwrap();

        let out = run(SearchOpts {
            cwd: p.to_path_buf(),
            pattern: Some("needle".into()),
            paths: vec!["**/*".into()],
            context_lines: 0,
            max_file_bytes: Some(DEFAULT_MAX_FILE_BYTES),
        })
        .unwrap();

        let hit_paths: Vec<&str> = out.hits.iter().map(|h| h.path.as_str()).collect();
        assert!(hit_paths.contains(&"ok.txt"), "text hit should be present, got {hit_paths:?}");
        assert!(
            !hit_paths.contains(&"blob.bin"),
            "binary file should not produce hits, got {hit_paths:?}",
        );
        let skipped_bin: Vec<&SkippedFile> =
            out.skipped.iter().filter(|s| s.path == "blob.bin").collect();
        assert_eq!(skipped_bin.len(), 1, "expected blob.bin in skipped: {:?}", out.skipped);
        assert_eq!(skipped_bin[0].reason, "binary");
    }

    #[test]
    fn skips_oversize_file() {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        // 2KB file — way over our 256-byte test limit.
        let big = "needle ".repeat(300);
        fs::write(p.join("huge.txt"), &big).unwrap();
        fs::write(p.join("small.txt"), "needle\n").unwrap();

        let out = run(SearchOpts {
            cwd: p.to_path_buf(),
            pattern: Some("needle".into()),
            paths: vec!["**/*".into()],
            context_lines: 0,
            max_file_bytes: Some(256),
        })
        .unwrap();

        let hit_paths: Vec<&str> = out.hits.iter().map(|h| h.path.as_str()).collect();
        assert!(hit_paths.contains(&"small.txt"));
        assert!(!hit_paths.contains(&"huge.txt"));
        let skipped: Vec<&SkippedFile> =
            out.skipped.iter().filter(|s| s.path == "huge.txt").collect();
        assert_eq!(skipped.len(), 1, "expected huge.txt skipped, got {:?}", out.skipped);
        assert_eq!(skipped[0].reason, "size");
        assert!(skipped[0].bytes.unwrap() > 256);
    }

    #[test]
    fn no_size_limit_when_none() {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        let big = "needle ".repeat(300);
        fs::write(p.join("huge.txt"), &big).unwrap();
        let out = run(SearchOpts {
            cwd: p.to_path_buf(),
            pattern: Some("needle".into()),
            paths: vec!["**/*".into()],
            context_lines: 0,
            max_file_bytes: None,
        })
        .unwrap();
        let hit_paths: Vec<&str> = out.hits.iter().map(|h| h.path.as_str()).collect();
        assert!(hit_paths.contains(&"huge.txt"));
        assert!(out.skipped.is_empty());
    }
}
