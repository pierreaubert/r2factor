//! The `Plan` type — a `module name -> [ItemId]` mapping with per-id
//! rationales. The deterministic build pipeline lives in [`build`] and the
//! human-readable dry-run printer in [`dry_run`].

mod build;
mod dry_run;

use crate::item::ItemId;
use std::collections::BTreeMap;

pub use build::build;
pub use dry_run::print_dry_run;

#[derive(Default, Debug)]
pub struct Plan {
    pub assignments: BTreeMap<String, Vec<ItemId>>,
    pub rationale: BTreeMap<ItemId, String>,
}

impl Plan {
    pub fn assign(&mut self, module: &str, id: ItemId, rationale: impl Into<String>) {
        self.assignments
            .entry(module.to_string())
            .or_default()
            .push(id);
        self.rationale.insert(id, rationale.into());
    }
}
