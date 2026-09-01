//! Next.js app-router file-convention routes. `app/**/page.tsx` → GET that path.

use crate::model::RouteFact;
use crate::plugin::RoutePlugin;

pub struct NextPlugin;

impl RoutePlugin for NextPlugin {
    fn routes(&self, path: &str, src: &str) -> Vec<RouteFact> {
        let Some(route_path) = file_to_route(path) else {
            return vec![];
        };
        let method = if path.contains("/route.") {
            // route.ts: look for exported GET/POST/…
            return http_exports(path, src, &route_path);
        } else {
            "GET"
        };
        let handler = handler_from_src(path, src);
        vec![RouteFact {
            method: method.into(),
            path: route_path,
            handler_id: handler,
            file_path: path.to_string(),
            line: 1,
        }]
    }
}

fn file_to_route(path: &str) -> Option<String> {
    // match `app/.../page.tsx` or `src/app/.../page.tsx` (and route.ts)
    let lower = path.replace('\\', "/");
    let marker = if let Some(i) = lower.find("/app/") {
        i + 5
    } else if lower.starts_with("app/") {
        4
    } else {
        return None;
    };
    let rest = &lower[marker..];
    let file = rest.rsplit('/').next().unwrap_or(rest);
    let is_page = file.starts_with("page.");
    let is_route = file.starts_with("route.");
    if !is_page && !is_route {
        return None;
    }
    let dir = rest.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut segs: Vec<&str> = Vec::new();
    for s in dir.split('/') {
        if s.is_empty() || s == "app" {
            continue;
        }
        if s.starts_with('(') && s.ends_with(')') {
            continue; // route group
        }
        if s.starts_with('@') {
            continue; // parallel route
        }
        segs.push(s);
    }
    let mut out = String::from("/");
    for (i, s) in segs.iter().enumerate() {
        if i > 0 {
            out.push('/');
        }
        if s.starts_with('[') && s.ends_with(']') {
            let inner = &s[1..s.len() - 1];
            if let Some(rest) = inner.strip_prefix("...") {
                out.push(':');
                out.push_str(rest);
                out.push('*');
            } else {
                out.push(':');
                out.push_str(inner);
            }
        } else {
            out.push_str(s);
        }
    }
    if out.is_empty() {
        out.push('/');
    }
    Some(out)
}

fn http_exports(path: &str, src: &str, route_path: &str) -> Vec<RouteFact> {
    const VERBS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        let t = line.trim_start();
        for v in VERBS {
            if t.contains(&format!("export async function {v}"))
                || t.contains(&format!("export function {v}"))
                || t.contains(&format!("export const {v}"))
            {
                out.push(RouteFact {
                    method: (*v).into(),
                    path: route_path.to_string(),
                    handler_id: crate::model::Symbol::make_id(path, v, i + 1),
                    file_path: path.to_string(),
                    line: i + 1,
                });
            }
        }
    }
    out
}

fn handler_from_src(path: &str, src: &str) -> String {
    for (i, line) in src.lines().enumerate() {
        let t = line.trim_start();
        for name in ["default", "Page", "GET", "POST"] {
            if t.contains(&format!("function {name}"))
                || t.contains(&format!("const {name}"))
                || t.contains("export default")
            {
                return crate::model::Symbol::make_id(path, name, i + 1);
            }
        }
    }
    crate::model::Symbol::module_id(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_page_becomes_get_route() {
        let rs = NextPlugin.routes("src/app/users/[id]/page.tsx", "export default function Page() {}");
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].method, "GET");
        assert_eq!(rs[0].path, "/users/:id");
    }

    #[test]
    fn route_ts_exports_verbs() {
        let src = "export async function GET() {}\nexport async function POST() {}";
        let rs = NextPlugin.routes("app/api/hello/route.ts", src);
        assert!(rs.iter().any(|r| r.method == "GET" && r.path == "/api/hello"));
        assert!(rs.iter().any(|r| r.method == "POST"));
    }
}
