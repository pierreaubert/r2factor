use super::Plan;
use crate::item::{ItemId, ParsedItem};
use serde::Serialize;
use std::collections::BTreeMap;

/// JSON-friendly cohesion summary. `score = intra / (intra + inter)` —
/// 1.0 means every ref stays inside its bucket, 0.0 means the split
/// shredded the reference graph. `top_cross_edges` lists the heaviest
/// `from -> to` bucket pairs (capped at 5) so callers can spot the
/// buckets that should probably merge or share a visibility lift.
#[derive(Debug, Clone, Serialize)]
pub struct CohesionReport {
    pub intra: usize,
    pub inter: usize,
    pub score: f64,
    pub top_cross_edges: Vec<CrossEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CrossEdge {
    pub from: String,
    pub to: String,
    pub weight: usize,
}

/// Pure version of [`report_cohesion`] for callers that need the data
/// instead of stdout.
pub fn cohesion_report(plan: &Plan, items: &[ParsedItem]) -> CohesionReport {
    let bucket_of: BTreeMap<ItemId, &String> = plan
        .assignments
        .iter()
        .flat_map(|(b, ids)| ids.iter().map(move |id| (*id, b)))
        .collect();
    let name_to_id: BTreeMap<&str, ItemId> = items
        .iter()
        .filter(|i| !i.name.is_empty())
        .map(|i| (i.name.as_str(), i.id))
        .collect();

    let mut intra: usize = 0;
    let mut inter: usize = 0;
    let mut inter_pairs: BTreeMap<(String, String), usize> = BTreeMap::new();
    for it in items {
        let Some(my_bucket) = bucket_of.get(&it.id) else {
            continue;
        };
        for r in &it.refs {
            let Some(target_id) = name_to_id.get(r.as_str()) else {
                continue;
            };
            let Some(target_bucket) = bucket_of.get(target_id) else {
                continue;
            };
            if my_bucket == target_bucket {
                intra += 1;
            } else {
                inter += 1;
                let key = ((*my_bucket).clone(), (*target_bucket).clone());
                *inter_pairs.entry(key).or_default() += 1;
            }
        }
    }

    let total = intra + inter;
    let score = if total == 0 {
        1.0
    } else {
        intra as f64 / total as f64
    };
    let mut heavy: Vec<((String, String), usize)> = inter_pairs.into_iter().collect();
    heavy.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let top_cross_edges = heavy
        .into_iter()
        .take(5)
        .map(|((from, to), weight)| CrossEdge { from, to, weight })
        .collect();
    CohesionReport {
        intra,
        inter,
        score,
        top_cross_edges,
    }
}

pub fn report_cohesion(plan: &Plan, items: &[ParsedItem]) {
    let r = cohesion_report(plan, items);
    println!(
        "cohesion: {intra} intra-bucket + {inter} cross-bucket refs (score {score:.2}; 1.0 = fully self-contained buckets)",
        intra = r.intra,
        inter = r.inter,
        score = r.score,
    );
    if !r.top_cross_edges.is_empty() {
        println!("cohesion: top cross-bucket edges:");
        for e in &r.top_cross_edges {
            println!("  {:>3}  {} -> {}", e.weight, e.from, e.to);
        }
    }
    println!();
}
