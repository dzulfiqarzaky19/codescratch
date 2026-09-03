//! Framework route plugins. Core stays unaware of Express/Next.
//! WP-3C.
//!
//! `RoutePlugin` lives here with `collect`, not in its own file: two adapters
//! (Express, Next) justify the trait; a 16-line extra file did not.

use crate::model::RouteFact;

pub mod express;
pub mod next;

/// A framework route plugin: emits routes from a source file. Concrete plugins
/// produce `route` nodes + `handles_route` edges.
pub trait RoutePlugin {
    fn routes(&self, path: &str, src: &str) -> Vec<RouteFact>;
}

fn all() -> Vec<Box<dyn RoutePlugin>> {
    vec![Box::new(express::ExpressPlugin), Box::new(next::NextPlugin)]
}

pub fn collect(path: &str, src: &str) -> Vec<RouteFact> {
    all()
        .into_iter()
        .flat_map(|p| p.routes(path, src))
        .collect()
}
