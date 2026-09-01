//! Extension seam for framework route plugins.
//!
//! A language `Extractor` trait used to live here too. It had one adapter and
//! zero call sites — Python was added with a `Lang` match inside
//! [`crate::extract::extract`], not through the trait — so it was a
//! hypothetical seam and has been deleted. `RoutePlugin` has two adapters
//! (Express, Next) and a real call site, so it stays.

use crate::model::RouteFact;

/// A framework route plugin: emits routes from a source file. Core stays unaware
/// of Express/Next/etc. Concrete plugins produce `route` nodes +
/// `handles_route` edges.
pub trait RoutePlugin {
    fn routes(&self, path: &str, src: &str) -> Vec<RouteFact>;
}
