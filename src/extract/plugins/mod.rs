//! Framework route plugins. Core stays unaware of Express/Next.
//! WP-3C.

use crate::model::RouteFact;
use crate::plugin::RoutePlugin;

pub mod express;
pub mod next;

pub fn all() -> Vec<Box<dyn RoutePlugin>> {
    vec![Box::new(express::ExpressPlugin), Box::new(next::NextPlugin)]
}

pub fn collect(path: &str, src: &str) -> Vec<RouteFact> {
    all().into_iter().flat_map(|p| p.routes(path, src)).collect()
}
