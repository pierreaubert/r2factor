use super::Plan;
use crate::item::{ItemId, ParsedItem};
use std::collections::BTreeMap;

/// Walk `items[i].refs`, resolve each ref name to an item id, and count how
/// many ref-edges stay inside their bucket vs cross to a different one.
/// Reports a quick cohesion summary at the end of the dry-run so the user
/// can judge whether the split is tight or fragmented before they accept
/// it. The score is `intra / (intra + inter)` — 1.0 means every ref stays
/// within its bucket, 0.0 means the split shredded the reference graph.
pub fn report_cohesion(plan: &Plan, items: &[ParsedItem]) {
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
                // Direction matters (a depends on b) so we keep ordered pairs.
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

    println!(
        "cohesion: {intra} intra-bucket + {inter} cross-bucket refs (score {score:.2}; 1.0 = fully self-contained buckets)"
    );
    if !inter_pairs.is_empty() {
        // Show the heaviest cross-bucket edges — these are the candidates
        // for either merging the two buckets or lifting visibility.
        let mut heavy: Vec<((String, String), usize)> = inter_pairs.into_iter().collect();
        heavy.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let show = heavy.iter().take(5);
        println!("cohesion: top cross-bucket edges:");
        for ((from, to), n) in show {
            println!("  {n:>3}  {from} -> {to}");
        }
    }
    println!();
}
