//! `codescratch fold` — grep output on stdin, hits grouped by enclosing symbol.
//! The search stays with the caller's grep (its flags, its regex dialect); this only decides
//! how much of the result is worth printing. Input that is small, not `path:line:text`, or
//! outside any graph passes through byte for byte, as does anything folding would not shrink.

use crate::db;
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// A raw result up to this size is already cheaper than any grouping of it.
const PASS_BELOW: usize = 500;
/// Symbol rows plus loose hit lines printed before the rest is only counted.
const MAX_ROWS: usize = 40;
const MAX_TEXT: usize = 120;
/// Hit line numbers listed per row.
const MAX_LINES: usize = 6;
/// Hit lines kept for a file the graph does not hold (no symbol to group under).
const MAX_LOOSE: usize = 3;
/// Input past this is streamed through untouched: a grep that walked `node_modules` is not
/// worth holding in memory, and a `| head` behind it must still be able to stop it.
const MAX_INPUT: u64 = 4 << 20;

#[derive(Clone, Debug)]
pub struct Span {
    pub kind: String,
    pub name: String,
    pub start: i64,
    pub end: i64,
}

#[derive(Debug, PartialEq)]
struct Hit<'a> {
    path: &'a str,
    line: i64,
    text: &'a str,
}

/// One appended `--log` line: what came in, what went out.
#[derive(Serialize, Debug, Default, PartialEq)]
pub struct Stats {
    pub raw_chars: usize,
    pub out_chars: usize,
    pub folded: bool,
    pub hits: usize,
    pub files: usize,
    pub symbols: usize,
}

/// `path:LINE:text` → hit. The path may hold colons; the first `:digits:` ends it.
fn parse_line(l: &str) -> Option<Hit<'_>> {
    for (i, _) in l.match_indices(':').filter(|(i, _)| *i > 0) {
        let rest = &l[i + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && rest.as_bytes().get(digits) == Some(&b':') {
            return Some(Hit {
                path: &l[..i],
                line: rest[..digits].parse().ok()?,
                text: &rest[digits + 1..],
            });
        }
    }
    None
}

/// Innermost span holding `line`: the shortest, and of equals the one starting last.
fn enclosing(spans: &[Span], line: i64) -> Option<usize> {
    spans
        .iter()
        .enumerate()
        .filter(|(_, s)| s.start <= line && line <= s.end)
        .min_by_key(|(_, s)| (s.end - s.start, -s.start))
        .map(|(i, _)| i)
}

/// Where a hit sits in its file: inside span `n`, or outside every span.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Owner {
    Symbol(usize),
    TopLevel,
}

struct Group<'a> {
    owner: Owner,
    lines: Vec<i64>,
    text: &'a str,
}

struct FileHits<'a> {
    path: &'a str,
    spans: Option<Vec<Span>>,
    /// Graph file: one group per owner. File without spans: `loose` instead.
    groups: Vec<Group<'a>>,
    loose: Vec<Hit<'a>>,
}

fn clip(text: &str) -> &str {
    let t = text.trim();
    match t.char_indices().nth(MAX_TEXT) {
        Some((i, _)) => &t[..i],
        None => t,
    }
}

fn line_list(lines: &[i64]) -> String {
    let mut s = lines
        .iter()
        .take(MAX_LINES)
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if lines.len() > MAX_LINES {
        s.push_str(",…");
    }
    s
}

/// Raw grep output → the folded text and its stats, or the reason the raw text should be
/// printed as is. `spans_of` maps a path as grep printed it to that file's symbol spans
/// (`None`: the graph does not hold the file).
pub fn fold(
    raw: &str,
    spans_of: &mut dyn FnMut(&str) -> Option<Vec<Span>>,
) -> Result<(String, Stats), &'static str> {
    if raw.len() <= PASS_BELOW {
        return Err("small");
    }
    let mut files: Vec<FileHits> = Vec::new();
    let mut at: HashMap<&str, usize> = HashMap::new();
    let mut hits = 0;
    for l in raw.lines().filter(|l| !l.is_empty()) {
        let hit = parse_line(l).ok_or("unparsed")?;
        hits += 1;
        let fi = *at.entry(hit.path).or_insert_with(|| {
            files.push(FileHits {
                path: hit.path,
                spans: spans_of(hit.path).filter(|s| !s.is_empty()),
                groups: Vec::new(),
                loose: Vec::new(),
            });
            files.len() - 1
        });
        let f = &mut files[fi];
        let Some(spans) = &f.spans else {
            f.loose.push(hit);
            continue;
        };
        let owner = enclosing(spans, hit.line).map_or(Owner::TopLevel, Owner::Symbol);
        match f.groups.iter_mut().find(|g| g.owner == owner) {
            Some(g) => g.lines.push(hit.line),
            None => f.groups.push(Group {
                owner,
                lines: vec![hit.line],
                text: hit.text,
            }),
        }
    }
    if files.iter().all(|f| f.spans.is_none()) {
        return Err("no-graph");
    }

    let symbols: usize = files.iter().map(|f| f.groups.len()).sum();
    let mut out = format!(
        "fold: {hits} hits in {} files, by enclosing symbol (repeat the grep once for raw lines)\n",
        files.len()
    );
    let mut rows = 0;
    let (mut cut_rows, mut cut_files) = (0, 0);
    for f in &files {
        let total = f.groups.len() + f.loose.len().min(MAX_LOOSE);
        if rows >= MAX_ROWS {
            cut_rows += total;
            cut_files += 1;
            continue;
        }
        out.push_str(f.path);
        out.push('\n');
        if let Some(spans) = &f.spans {
            for (n, g) in f.groups.iter().enumerate() {
                if rows >= MAX_ROWS {
                    cut_rows += f.groups.len() - n;
                    cut_files += 1;
                    break;
                }
                rows += 1;
                let head = match g.owner {
                    Owner::Symbol(i) => {
                        let s = &spans[i];
                        format!("{}-{} {} {}", s.start, s.end, s.kind, s.name)
                    }
                    Owner::TopLevel => "(top level)".to_string(),
                };
                out.push_str(&format!(
                    "  {head} ×{} L{}: {}\n",
                    g.lines.len(),
                    line_list(&g.lines),
                    clip(g.text)
                ));
            }
        } else {
            for h in f.loose.iter().take(MAX_LOOSE) {
                rows += 1;
                out.push_str(&format!("  L{}: {}\n", h.line, clip(h.text)));
            }
            if f.loose.len() > MAX_LOOSE {
                out.push_str(&format!("  +{} more\n", f.loose.len() - MAX_LOOSE));
            }
        }
    }
    if cut_rows > 0 {
        out.push_str(&format!("… +{cut_rows} more rows in {cut_files} files\n"));
    }
    if out.len() >= raw.len() {
        return Err("not-smaller");
    }
    let stats = Stats {
        raw_chars: raw.len(),
        out_chars: out.len(),
        folded: true,
        hits,
        files: files.len(),
        symbols,
    };
    Ok((out, stats))
}

/// What reads on after `fold` in the caller's pipe: `| head -N` keeps the first N lines,
/// `| cut -c LIST` keeps those characters of each line. Default: everything.
#[derive(Default)]
pub struct Tail {
    pub head: Option<usize>,
    /// 1-based inclusive ranges, as `cut -c` reads them.
    pub cut: Option<Vec<(usize, usize)>>,
}

impl Tail {
    /// `cut -c` LIST: `N`, `N-`, `-M`, `N-M`, comma-separated. None when it is not one.
    pub fn parse_cut(list: &str) -> Option<Vec<(usize, usize)>> {
        list.split(',')
            .map(|r| {
                let (a, b) = r.split_once('-').unwrap_or((r, r));
                let a = if a.is_empty() { 1 } else { a.parse().ok()? };
                let b = if b.is_empty() { usize::MAX } else { b.parse().ok()? };
                (a >= 1 && a <= b).then_some((a, b))
            })
            .collect()
    }

    /// `text` as it leaves the tail.
    pub fn view(&self, text: &str) -> String {
        let lines = text.split_inclusive('\n').take(self.head.unwrap_or(usize::MAX));
        let Some(cut) = &self.cut else {
            return lines.collect();
        };
        lines
            .map(|l| {
                let (body, nl) = l.strip_suffix('\n').map_or((l, ""), |b| (b, "\n"));
                let kept: String = body
                    .chars()
                    .enumerate()
                    .filter(|(i, _)| cut.iter().any(|&(a, b)| a <= i + 1 && i + 1 <= b))
                    .map(|(_, c)| c)
                    .collect();
                kept + nl
            })
            .collect()
    }
}

/// `raw` with a leading `file:` taken off every line that has one, newlines kept.
fn strip_prefix(raw: &str, file: &str) -> String {
    let prefix = format!("{file}:");
    raw.split_inclusive('\n')
        .map(|l| l.strip_prefix(prefix.as_str()).unwrap_or(l))
        .collect()
}

/// Symbol spans read straight from each repo's graph, one read-only connection per root.
struct Graphs {
    cwd: PathBuf,
    conns: HashMap<PathBuf, Option<Connection>>,
}

impl Graphs {
    fn spans(&mut self, path: &str) -> Option<Vec<Span>> {
        let abs = std::fs::canonicalize(self.cwd.join(path)).ok()?;
        let root = abs.ancestors().skip(1).find(|d| db::exists(d))?.to_path_buf();
        let rel = abs.strip_prefix(&root).ok()?.to_string_lossy().replace('\\', "/");
        let conn = self
            .conns
            .entry(root.clone())
            .or_insert_with(|| db::open_read(&root).ok())
            .as_ref()?;
        // Communities and processes are graph-level nodes, not source ranges.
        let mut stmt = conn
            .prepare_cached(
                "SELECT kind, name, start_line, end_line FROM nodes
                 WHERE file_path = ?1 AND start_line > 0 AND kind NOT IN ('community', 'process')",
            )
            .ok()?;
        let rows = stmt
            .query_map([rel], |r| {
                Ok(Span {
                    kind: r.get(0)?,
                    name: r.get(1)?,
                    start: r.get(2)?,
                    end: r.get(3)?,
                })
            })
            .ok()?;
        Some(rows.flatten().collect())
    }
}

/// One `--log` line. `tag` is the caller's name for this search; `pass` says why the raw
/// text went out unchanged.
#[derive(Serialize)]
struct LogLine<'a> {
    ts: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<&'a str>,
    #[serde(flatten)]
    stats: &'a Stats,
    #[serde(skip_serializing_if = "Option::is_none")]
    pass: Option<&'a str>,
}

fn append_log(log: &Path, line: &LogLine) -> Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    writeln!(f, "{}", serde_json::to_string(line)?)?;
    Ok(())
}

/// stdin → stdout. `false` when stdin was empty: the grep found nothing, and the caller exits 1
/// so `grep X | codescratch fold && next` still behaves like `grep X && next`.
pub fn run(
    log: Option<&Path>,
    tag: Option<&str>,
    strip_path: Option<&str>,
    tail: &Tail,
) -> Result<bool> {
    let mut stdin = std::io::stdin().lock();
    let mut bytes = Vec::new();
    stdin.by_ref().take(MAX_INPUT + 1).read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        return Ok(false);
    }
    let raw = String::from_utf8_lossy(&bytes);
    let mut graphs = Graphs {
        // No cwd, no graph: relative paths then resolve nowhere and the input passes through.
        cwd: std::env::current_dir().unwrap_or_default(),
        conns: HashMap::new(),
    };
    let huge = bytes.len() as u64 > MAX_INPUT;
    // What the grep would have printed without the caller's `-H`: the size to beat, and the
    // bytes to pass through.
    let plain = match strip_path {
        Some(file) if !huge => Cow::Owned(strip_prefix(&raw, file)),
        _ => Cow::Borrowed(raw.as_ref()),
    };
    // Sizes as the reader sees them: both answers still go through the caller's `| head` /
    // `| cut -c`, so a capped grep is the size to beat, and the size the log records.
    let shown = tail.view(&plain).len();
    let folded = if huge {
        Err("huge")
    } else if shown <= PASS_BELOW {
        Err("small")
    } else {
        fold(&raw, &mut |p| graphs.spans(p)).and_then(|(out, mut stats)| {
            let out_shown = tail.view(&out).len();
            if out_shown >= shown {
                return Err("not-smaller");
            }
            stats.raw_chars = shown;
            stats.out_chars = out_shown;
            Ok((out, stats))
        })
    };
    let passed = Stats {
        raw_chars: shown,
        out_chars: shown,
        ..Stats::default()
    };
    if let Some(log) = log {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let line = LogLine {
            ts,
            tag,
            stats: folded.as_ref().map_or(&passed, |(_, s)| s),
            pass: folded.as_ref().err().copied(),
        };
        // The log is a measurement aid: failing to write it must not cost the answer.
        let _ = append_log(log, &line);
    }
    // A closed pipe (`| head`) is the reader's choice, not an error.
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(match &folded {
        Ok((out, _)) => out.as_bytes(),
        Err(_) if strip_path.is_some() && !huge => plain.as_bytes(),
        Err(_) => &bytes,
    });
    if huge {
        let _ = std::io::copy(&mut stdin, &mut stdout);
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(kind: &str, name: &str, start: i64, end: i64) -> Span {
        Span {
            kind: kind.to_string(),
            name: name.to_string(),
            start,
            end,
        }
    }

    /// `n` hits in `path` on lines 1..=n, long enough together to clear `PASS_BELOW`.
    fn hits(path: &str, n: i64) -> String {
        (1..=n)
            .map(|i| format!("{path}:{i}:  const value{i} = computeSomething(argument{i});\n"))
            .collect()
    }

    #[test]
    fn parses_path_line_text() {
        assert_eq!(
            parse_line("src/a.ts:12:  foo(1)"),
            Some(Hit {
                path: "src/a.ts",
                line: 12,
                text: "  foo(1)"
            })
        );
        assert_eq!(parse_line("C:/x/a.ts:7:b:c").map(|h| (h.path, h.line)), Some(("C:/x/a.ts", 7)));
    }

    #[test]
    fn rejects_lines_that_are_not_hits() {
        assert_eq!(parse_line("--"), None);
        assert_eq!(parse_line("src/a.ts-12-  context"), None);
        assert_eq!(parse_line("Binary file a.bin matches"), None);
        assert_eq!(parse_line("12:line of a single-file grep"), None);
    }

    #[test]
    fn enclosing_picks_the_innermost_span() {
        let spans = [span("class", "A", 1, 50), span("method", "m", 10, 20), span("const", "c", 12, 12)];
        assert_eq!(enclosing(&spans, 12), Some(2));
        assert_eq!(enclosing(&spans, 15), Some(1));
        assert_eq!(enclosing(&spans, 40), Some(0));
        assert_eq!(enclosing(&spans, 60), None);
    }

    #[test]
    fn tail_keeps_what_head_and_cut_keep() {
        let tail = Tail {
            head: Some(2),
            cut: Tail::parse_cut("1-3,5"),
        };
        assert_eq!(tail.view("abcdef\nghijkl\nmno\n"), "abce\nghik\n");
        assert_eq!(Tail::default().view("a\nb"), "a\nb");
        assert_eq!(Tail::parse_cut("-150"), Some(vec![(1, 150)]));
        assert_eq!(Tail::parse_cut("7-"), Some(vec![(7, usize::MAX)]));
        assert_eq!(Tail::parse_cut("3-1"), None);
        assert_eq!(Tail::parse_cut("x"), None);
    }

    #[test]
    fn strip_prefix_drops_only_the_added_path() {
        let raw = "src/a.ts:3:x\nsrc/a.ts:9:src/a.ts:y\nBinary file src/a.ts matches\n";
        assert_eq!(strip_prefix(raw, "src/a.ts"), "3:x\n9:src/a.ts:y\nBinary file src/a.ts matches\n");
    }

    #[test]
    fn small_input_passes_through() {
        let raw = hits("src/a.ts", 3);
        assert!(raw.len() <= PASS_BELOW);
        assert_eq!(fold(&raw, &mut |_| Some(vec![span("function", "f", 1, 99)])).err(), Some("small"));
    }

    #[test]
    fn unparseable_line_passes_everything_through() {
        let raw = format!("{}--\n", hits("src/a.ts", 20));
        assert_eq!(fold(&raw, &mut |_| Some(vec![span("function", "f", 1, 99)])).err(), Some("unparsed"));
    }

    #[test]
    fn no_graph_file_passes_through() {
        assert_eq!(fold(&hits("docs/a.md", 20), &mut |_| None).err(), Some("no-graph"));
    }

    #[test]
    fn groups_hits_by_symbol_and_top_level() {
        let raw = hits("src/a.ts", 20);
        let (out, stats) = fold(&raw, &mut |_| {
            Some(vec![span("function", "first", 3, 10), span("function", "second", 11, 20)])
        })
        .unwrap();
        assert!(out.contains("src/a.ts\n"), "{out}");
        assert!(out.contains("  (top level) ×2 L1,2: const value1 = computeSomething(argument1);"), "{out}");
        assert!(out.contains("  3-10 function first ×8 L3,4,5,6,7,8,…: const value3"), "{out}");
        assert!(out.contains("  11-20 function second ×10 "), "{out}");
        assert!(out.len() < raw.len());
        assert_eq!((stats.hits, stats.files, stats.symbols, stats.folded), (20, 1, 3, true));
        assert_eq!((stats.raw_chars, stats.out_chars), (raw.len(), out.len()));
    }

    #[test]
    fn file_outside_the_graph_keeps_a_few_lines() {
        let raw = format!("{}{}", hits("src/a.ts", 12), hits("docs/a.md", 8));
        let (out, _) = fold(&raw, &mut |p| {
            p.ends_with(".ts").then(|| vec![span("function", "f", 1, 99)])
        })
        .unwrap();
        assert!(out.contains("docs/a.md\n  L1: const value1"), "{out}");
        assert!(out.contains("  +5 more\n"), "{out}");
    }

    #[test]
    fn caps_rows_and_counts_the_rest() {
        let raw: String = (1..=60).map(|i| hits(&format!("src/f{i}.ts"), 2)).collect();
        let (out, _) = fold(&raw, &mut |_| Some(vec![span("function", "f", 1, 99)])).unwrap();
        assert_eq!(out.matches(" function f ").count(), MAX_ROWS);
        assert!(out.ends_with("… +20 more rows in 20 files\n"), "{out}");
    }

    #[test]
    fn output_not_smaller_than_input_passes_through() {
        // One hit per file: a header line per file makes the fold longer than the grep.
        let raw: String = (1..=30).map(|i| format!("src/file{i}.ts:1:x{i}\n")).collect();
        assert!(raw.len() > PASS_BELOW);
        assert_eq!(fold(&raw, &mut |_| Some(vec![span("function", "f", 1, 99)])).err(), Some("not-smaller"));
    }
}
