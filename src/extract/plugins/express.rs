//! Express / typical `app.get('/path', handler)` route plugin.

use crate::model::RouteFact;
use crate::plugin::RoutePlugin;

pub struct ExpressPlugin;

const METHODS: &[&str] = &["get", "post", "put", "patch", "delete", "options", "head", "all", "use"];

impl RoutePlugin for ExpressPlugin {
    fn name(&self) -> &str {
        "express"
    }
    fn routes(&self, path: &str, src: &str) -> Vec<RouteFact> {
        let mut out = Vec::new();
        for (i, line) in src.lines().enumerate() {
            let ln = i + 1;
            let t = line.trim_start();
            // app.get('/x', handler)  |  router.post("/x", fn)
            let Some(rest) = strip_receiver_method(t) else { continue };
            let (method, after) = rest;
            let Some((route_path, handler)) = split_path_and_handler(after) else { continue };
            let handler_id = crate::model::Symbol::make_id(path, &handler, ln);
            out.push(RouteFact {
                method: method.to_uppercase(),
                path: route_path,
                handler_id,
                file_path: path.to_string(),
                line: ln,
            });
        }
        out
    }
}

fn strip_receiver_method(t: &str) -> Option<(&str, &str)> {
    // look for `.METHOD(` after an identifier (app/router/this)
    for m in METHODS {
        let needle = format!(".{m}(");
        if let Some(i) = t.find(&needle) {
            let before = &t[..i];
            if before.chars().last().map(|c| c.is_ascii_alphanumeric() || c == '_' || c == ')').unwrap_or(false) {
                return Some((m, &t[i + needle.len()..]));
            }
        }
    }
    None
}

fn split_path_and_handler(after: &str) -> Option<(String, String)> {
    let after = after.trim_start();
    let q = after.chars().next()?;
    if q != '\'' && q != '"' && q != '`' {
        return None;
    }
    let rest = &after[1..];
    let end = rest.find(q)?;
    let route = rest[..end].to_string();
    let after_path = rest[end + 1..].trim_start();
    let after_path = after_path.strip_prefix(',').unwrap_or(after_path).trim_start();
    let handler = handler_name(after_path)?;
    Some((route, handler))
}

fn handler_name(s: &str) -> Option<String> {
    let s = s.trim_start();
    if s.starts_with("async ") || s.starts_with("function") || s.starts_with('(') {
        return Some("<anon>".into());
    }
    let ident: String = s
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    if ident.is_empty() {
        Some("<anon>".into())
    } else {
        Some(ident.rsplit('.').next().unwrap_or(&ident).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_app_get() {
        let src = r#"
import express from "express";
const app = express();
function listUsers(req, res) { res.send(1); }
app.get('/users', listUsers);
app.post("/users/:id", (req, res) => {});
"#;
        let rs = ExpressPlugin.routes("src/app.ts", src);
        assert!(rs.iter().any(|r| r.method == "GET" && r.path == "/users"));
        assert!(rs.iter().any(|r| r.method == "POST" && r.path == "/users/:id"));
    }
}
