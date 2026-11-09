//! Project main and language localization pickle resources into native rows.
//! Client `localization/localizationBase.py` loads the packaged localization data.
//! Language identity comes from resource provenance; the full tagged graph is retained.
//! No fallback translations are introduced; unsupported pickle or row shapes fail.
use anyhow::{bail, Context, Result};
use serde_json::{Map, Value};
use super::pickle::{self, Graph, Node};

/// Native rows and the full tagged pickle graph; no fallback translations are introduced.
pub struct DecodedLocalization {
    pub records: Vec<Value>,
    pub metadata: Value,
}

// Converting shared pickle references into JSON must itself have a finite expansion budget.
fn comparable(graph: &Graph, index: usize, depth: usize, remaining: &mut usize) -> Result<Value> {
    if depth > 128 || *remaining == 0 { bail!("localization projection budget exceeded"); }
    *remaining -= 1;
    let node = graph.nodes.get(index).context("invalid pickle node index")?;
    Ok(match node {
        Node::None => Value::Null,
        Node::Bool(v) => Value::Bool(*v),
        Node::Int(v) => {
            if let Ok(i) = v.parse::<i64>() { Value::from(i) }
            else if let Ok(i) = v.parse::<u64>() { Value::from(i) }
            else { bail!("localization integer outside JSON value domain") }
        }
        Node::Float(v) => Value::Number(serde_json::Number::from_f64(*v).context("nonfinite localization number")?),
        Node::Text(v) => Value::String(v.clone()),
        Node::List(items) | Node::Tuple(items) => Value::Array(items.iter().map(|&i| comparable(graph, i, depth + 1, remaining)).collect::<Result<Vec<_>>>()?),
        Node::Dict(items) => {
            let mut out = Map::new();
            for &(key, value) in items {
                let key = match graph.nodes.get(key).context("invalid pickle key index")? {
                    Node::Text(s) | Node::Int(s) => s.clone(),
                    _ => bail!("non-JSON localization map key"),
                };
                if out.insert(key, comparable(graph, value, depth + 1, remaining)?).is_some() { bail!("colliding localization JSON map keys"); }
            }
            Value::Object(out)
        }
    })
}

pub fn decode(family: &str, raw: &[u8]) -> Result<DecodedLocalization> {
    if !matches!(family, "localization_fsd_main" | "localization_fsd_en_us" | "localization_fsd_zh") { bail!("unsupported localization family"); }
    let graph = pickle::decode(raw).map_err(anyhow::Error::msg)?;
    let metadata = pickle::tagged_json(&graph).map_err(anyhow::Error::msg)?;
    let mut remaining = 50_000_000usize;
    let root = graph.nodes.get(graph.root).context("missing localization root")?;
    let mut records = Vec::new();
    if family == "localization_fsd_main" {
        let Node::Dict(items) = root else { bail!("main localization root is not a map") };
        for &(key, value) in items {
            let Node::Text(key) = graph.nodes.get(key).context("invalid main localization key")? else { bail!("main localization key is not text") };
            records.push(serde_json::json!({"_key": key, "text": comparable(&graph, value, 0, &mut remaining)?}));
        }
    } else {
        let (Node::List(parts) | Node::Tuple(parts)) = root else { bail!("language localization root is not a sequence") };
        if parts.len() != 2 { bail!("language localization root must have two elements"); }
        let Node::Dict(items) = graph.nodes.get(parts[1]).context("invalid language message map")? else { bail!("language messages are not a map") };
        for &(key, value) in items {
            let Node::Int(key) = graph.nodes.get(key).context("invalid message key")? else { bail!("message key is not integer") };
            let id: i64 = key.parse().context("message key outside signed integer domain")?;
            let Node::Tuple(parts) = graph.nodes.get(value).context("invalid message value")? else { bail!("message value is not a tuple") };
            if parts.len() != 3 { bail!("message tuple must have three elements"); }
            let Node::Text(text) = graph.nodes.get(parts[0]).context("invalid message text")? else { bail!("message text is not a string") };
            records.push(serde_json::json!({"_key": id, "text": text, "metadata": comparable(&graph, parts[1], 0, &mut remaining)?, "tokens": comparable(&graph, parts[2], 0, &mut remaining)?}));
        }
    }
    Ok(DecodedLocalization { records, metadata })
}

#[cfg(test)]
mod tests;
