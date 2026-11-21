//! Sources shipped with the tool.

pub mod dst;

use crate::source::Registry;

/// The one place a new agent gets wired in: write a `SessionSource` and add it
/// here. Order is the order the UI cycles through with `s`.
pub fn registry() -> Registry {
    let mut registry = Registry::new();
    registry.register(dst::Dst);
    registry
}
