//! Search engine: ripgrep-grade walking + matching, pure Rust (no shell-out to `rg`).
//! Returns ranked snippets across matched files.

use anyhow::Result;
use grep_regex::RegexMatcher;
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use crate::walk::{Walk, relativize};

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

pub struct SearchOpts {
    pub cwd: PathBuf,
    pub pattern: Option<String>,
    pub paths: Vec<String>,
    pub context_lines: u32,
}

pub fn run(opts: SearchOpts) -> Result<Vec<SearchHit>> {
    let walk = Walk::new(opts.cwd.clone(), opts.paths.clone());
    let files = walk.files()?;

    let Some(pattern) = opts.pattern else {
        return Ok(files
            .iter()
            .map(|abs| SearchHit {
                path: relativize(&opts.cwd, abs),
                line_start: 0,
                line_end: 0,
                snippet: String::new(),
                match_count: 0,
            })
            .collect());
    };

    let matcher = RegexMatcher::new_line_matcher(&pattern)?;
    let mut searcher = SearcherBuilder::new()
        .before_context(opts.context_lines as usize)
        .after_context(opts.context_lines as usize)
        .line_number(true)
        .build();

    let mut hits = Vec::new();
    for abs in &files {
        let mut sink = HitSink::new();
        if searcher.search_path(&matcher, abs, &mut sink).is_err() {
            continue;
        }
        for snippet in sink.into_snippets(opts.context_lines as u32) {
            hits.push(SearchHit {
                path: relativize(&opts.cwd, abs),
                line_start: snippet.line_start,
                line_end: snippet.line_end,
                snippet: snippet.text,
                match_count: snippet.match_count,
            });
        }
    }
    Ok(hits)
}

struct HitSink {
    /// All lines we received from the searcher, keyed by line number (1-based).
    lines: BTreeMap<u32, String>,
    match_lines: HashSet<u32>,
}

struct GroupedSnippet {
    line_start: u32,
    line_end: u32,
    text: String,
    match_count: u32,
}

impl HitSink {
    fn new() -> Self {
        Self { lines: BTreeMap::new(), match_lines: HashSet::new() }
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
        let mut iter = self.lines.keys().copied().collect::<Vec<_>>();
        iter.sort_unstable();

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
        let hits = run(SearchOpts {
            cwd: p.to_path_buf(),
            pattern: Some(r"\bfoo\b".into()),
            paths: vec!["**/*.ts".into()],
            context_lines: 1,
        })
        .unwrap();
        assert!(!hits.is_empty(), "expected hits");
        // Found in two locations.
        let total_matches: u32 = hits.iter().map(|h| h.match_count).sum();
        assert!(total_matches >= 2, "got total_matches={total_matches}");
    }

    #[test]
    fn glob_only_no_pattern() {
        let dir = TempDir::new().unwrap();
        let p = dir.path();
        fs::write(p.join("a.ts"), "x").unwrap();
        fs::write(p.join("b.txt"), "x").unwrap();
        let hits = run(SearchOpts {
            cwd: p.to_path_buf(),
            pattern: None,
            paths: vec!["**/*.ts".into()],
            context_lines: 0,
        })
        .unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert!(paths.contains(&"a.ts"));
        assert!(!paths.contains(&"b.txt"));
    }
}

