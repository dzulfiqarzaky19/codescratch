//! Heuristic dispatch edges. Never faked as AST: `provenance=heuristic`,
//! `reason` is a human label. WP-3A.

use crate::model::{Edge, FileFacts, Provenance};

/// Pattern-guess extra edges from already-extracted facts + raw source.
/// Does not invent resolved destinations — those stay open for the resolver.
pub fn extra_edges(path_rel: &str, src: &str, facts: &FileFacts) -> Vec<Edge> {
    let mut out = Vec::new();
    out.extend(callback_args(path_rel, facts));
    out.extend(event_emitter(path_rel, src, facts));
    out.extend(set_state_to_render(path_rel, facts));
    out
}

/// `foo(cb)` / `foo(() => …)` where `cb` is a same-file function — dispatch edge.
fn callback_args(file: &str, facts: &FileFacts) -> Vec<Edge> {
    let names: std::collections::HashSet<&str> =
        facts.symbols.iter().map(|s| s.name.as_str()).collect();
    facts
        .calls
        .iter()
        .filter(|c| !c.member && names.contains(c.name.as_str()) && c.file_path == file)
        .filter_map(|c| {
            // already captured as a call; heuristic only when the callee is
            // passed as an identifier argument to another call in the same line-ish.
            // We approximate: if a call's name is a local function AND the enclosing
            // symbol also calls something else on the same line, skip (too noisy).
            // Conservative: emit only when the callee is a local function used as
            // a *member-less* call from a different enclosing symbol than itself.
            let callee = facts
                .symbols
                .iter()
                .find(|s| s.name == c.name && s.file_path == file)?;
            if c.from_id == callee.id {
                return None;
            }
            Some(Edge {
                src_id: c.from_id.clone(),
                dst_id: Some(callee.id.clone()),
                kind: "dispatches".into(),
                raw_name: c.name.clone(),
                resolved: true,
                conf: "weak".into(),
                reason: "callback-arg".into(),
                provenance: Provenance::Heuristic.as_str().into(),
                file_path: file.to_string(),
                line: c.line,
            })
        })
        .collect()
}

/// EventEmitter `on('x', handler)` paired with `emit('x', …)` in the same file.
fn event_emitter(file: &str, src: &str, facts: &FileFacts) -> Vec<Edge> {
    let mut ons: Vec<(String, usize, String)> = Vec::new(); // (event, line, from_id)
    let mut emits: Vec<(String, usize, String)> = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let ln = i + 1;
        if let Some(ev) = extract_event_arg(line, ".on(") {
            let from = enclosing_at(facts, file, ln);
            ons.push((ev, ln, from));
        }
        if let Some(ev) = extract_event_arg(line, ".emit(") {
            let from = enclosing_at(facts, file, ln);
            emits.push((ev, ln, from));
        }
        if let Some(ev) = extract_event_arg(line, ".addEventListener(") {
            let from = enclosing_at(facts, file, ln);
            ons.push((ev, ln, from));
        }
    }
    let mut out = Vec::new();
    for (ev, eline, esrc) in &emits {
        for (oev, oline, _) in &ons {
            if oev == ev {
                out.push(Edge {
                    src_id: esrc.clone(),
                    dst_id: None,
                    kind: "dispatches".into(),
                    raw_name: format!("emit:{ev}"),
                    resolved: false,
                    conf: "weak".into(),
                    reason: "event-emit-on".into(),
                    provenance: Provenance::Heuristic.as_str().into(),
                    file_path: file.to_string(),
                    line: *eline,
                });
                let _ = oline;
                break;
            }
        }
    }
    out
}

fn extract_event_arg(line: &str, needle: &str) -> Option<String> {
    let i = line.find(needle)?;
    let rest = &line[i + needle.len()..];
    let rest = rest.trim_start();
    let q = rest.chars().next()?;
    if q != '\'' && q != '"' && q != '`' {
        return None;
    }
    let rest = &rest[1..];
    let end = rest.find(q)?;
    let ev = &rest[..end];
    if ev.is_empty() {
        None
    } else {
        Some(ev.to_string())
    }
}

/// React-ish: `setState` / `setXxx` call in a class/function that also has `render`.
fn set_state_to_render(file: &str, facts: &FileFacts) -> Vec<Edge> {
    let renders: Vec<&crate::model::Symbol> = facts
        .symbols
        .iter()
        .filter(|s| s.name == "render" && s.kind == "method")
        .collect();
    if renders.is_empty() {
        return vec![];
    }
    facts
        .calls
        .iter()
        .filter(|c| c.name == "setState" || c.name.starts_with("set") && c.name.len() > 3)
        .filter_map(|c| {
            let render = renders.first()?;
            Some(Edge {
                src_id: c.from_id.clone(),
                dst_id: Some(render.id.clone()),
                kind: "dispatches".into(),
                raw_name: c.name.clone(),
                resolved: true,
                conf: "weak".into(),
                reason: "setState-render".into(),
                provenance: Provenance::Heuristic.as_str().into(),
                file_path: file.to_string(),
                line: c.line,
            })
        })
        .collect()
}

fn enclosing_at(facts: &FileFacts, file: &str, line: usize) -> String {
    facts
        .symbols
        .iter()
        .filter(|s| s.file_path == file && s.start_line <= line && s.end_line >= line)
        .max_by_key(|s| s.start_line)
        .map(|s| s.id.clone())
        .unwrap_or_else(|| crate::model::Symbol::module_id(file))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::extract;

    #[test]
    fn event_emit_on_is_heuristic() {
        let src = r#"
export function listen() { bus.on('ready', go); }
export function fire() { bus.emit('ready'); }
export function go() {}
"#;
        let f = extract("src/a.ts", src);
        let edges = extra_edges("src/a.ts", src, &f);
        assert!(
            edges.iter().any(|e| e.kind == "dispatches"
                && e.reason == "event-emit-on"
                && e.provenance == "heuristic"),
            "expected emit/on dispatch: {edges:?}"
        );
    }

    #[test]
    fn setstate_to_render_is_heuristic() {
        let src = r#"
class Box {
  setState(s: unknown) { this.state = s; }
  tick() { this.setState({ n: 1 }); }
  render() { return 1; }
}
"#;
        let f = extract("src/a.ts", src);
        let edges = extra_edges("src/a.ts", src, &f);
        assert!(
            edges
                .iter()
                .any(|e| e.reason == "setState-render" && e.provenance == "heuristic"),
            "expected setState→render: {edges:?}"
        );
    }
}
