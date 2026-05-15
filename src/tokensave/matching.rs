use crate::item::ParsedItem;
use tokensave::types::Node;

/// Pick the tokensave node that most tightly aligns with a `ParsedItem`.
/// Both line ranges are 1-indexed; we score by the distance between the
/// node's `attrs_start_line` and the item's `line_start`, requiring the node
/// to start within the item's span. The closest match wins.
pub fn best_node_for_item(it: &ParsedItem, nodes: &[Node]) -> Option<String> {
    let mut best: Option<(String, u32)> = None;
    for n in nodes {
        let s = n.attrs_start_line as usize;
        if s >= it.line_start && s <= it.line_end {
            let d = (s as i64 - it.line_start as i64).unsigned_abs() as u32;
            match &best {
                None => best = Some((n.id.clone(), d)),
                Some((_, bd)) if d < *bd => best = Some((n.id.clone(), d)),
                _ => {}
            }
        }
    }
    best.map(|(id, _)| id)
}
