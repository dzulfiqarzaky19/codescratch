//! Extension seams (contract). Frozen in Wave 0 so later workers slot in without
//! touching call sites: WP-4A (Python) implements `Extractor`, WP-3C (Express/Next)
//! implements `RoutePlugin`. Unused until those waves.
#![allow(dead_code)]

use crate::model::{FileFacts, Lang, RouteFact};

/// A language extractor: source → structural facts. TS/JS is the default impl.
pub trait Extractor {
    fn langs(&self) -> &[Lang];
    fn extract(&self, path: &str, src: &str) -> FileFacts;
}

/// Built-in TS/JS extractor — wraps [`crate::extract::extract`].
pub struct TsJsExtractor;

impl Extractor for TsJsExtractor {
    fn langs(&self) -> &[Lang] {
        const L: &[Lang] = &[Lang::Ts, Lang::Tsx, Lang::Js];
        L
    }
    fn extract(&self, path: &str, src: &str) -> FileFacts {
        crate::extract::extract(path, src)
    }
}

/// A framework route plugin: emits routes from a source file. Core stays unaware
/// of Express/Next/etc. WP-3C implements concrete plugins that produce
/// `route` nodes + `handles_route` edges.
pub trait RoutePlugin {
    fn name(&self) -> &str;
    fn routes(&self, path: &str, src: &str) -> Vec<RouteFact>;
}
