//! Interpret bounded data-only protocol 0–2 pickles into a retained graph.
//! Client `localization/localizationBase.py` consumes the localization pickle resources.
//! The supported empty `collections.OrderedDict` constructor is inert, never executed.
//! Other constructors, unsupported opcodes, cycles and inputs exceeding limits fail.
use std::collections::{HashMap, HashSet};
use serde_json::{json, Value};

const MAX_BYTES: usize = 128 * 1024 * 1024;
const MAX_OPS: usize = 50_000_000;
const MAX_ITEMS: usize = 2_000_000;
const MAX_MEMO: usize = 4_000_000;
const MAX_SCALAR: usize = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 128;
// The binary-to-decimal conversion is quadratic; reject larger values before conversion.
const MAX_BINARY_LONG: usize = 1024;
const MAX_JSON_NODES: usize = 50_000_000;
const MAX_JSON_BYTES: usize = 1024 * 1024 * 1024;
// Independent graph limits: at most 8M nodes, 16M references and 256 MiB
// retained scalar/key data. Vector/hash-table slack and validation scratch can
// roughly double these figures; the 75 MiB/340k-row source has ample headroom.
const MAX_GRAPH_NODES: usize = 8_000_000;
const MAX_GRAPH_EDGES: usize = 16_000_000;
const MAX_GRAPH_SCALARS: usize = 256 * 1024 * 1024;
const MARK: usize = usize::MAX;
const ORDERED: usize = usize::MAX - 1;

#[derive(Debug)]
pub struct Graph { pub root: usize, pub nodes: Vec<Node> }
#[derive(Debug)]
pub enum Node { None, Bool(bool), Int(String), Float(f64), Text(String), List(Vec<usize>), Tuple(Vec<usize>), Dict(Vec<(usize, usize)>) }

struct Parser<'a> { input: &'a [u8], pos: usize, stack: Vec<usize>, memo: HashMap<u32, usize>, nodes: Vec<Node>, dict_keys: HashMap<usize, HashSet<String>>, edges: usize, scalars: usize, limits: (usize, usize, usize) }
impl<'a> Parser<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("length overflow")?;
        let data = self.input.get(self.pos..end).ok_or("truncated pickle")?;
        self.pos = end; Ok(data)
    }
    fn byte(&mut self) -> Result<u8, String> { Ok(self.take(1)?[0]) }
    fn line(&mut self) -> Result<&'a [u8], String> {
        let rest = &self.input[self.pos..];
        let n = rest.iter().position(|b| *b == b'\n').ok_or("unterminated line")?;
        if n > MAX_SCALAR { return Err("scalar size limit".into()); }
        let line = self.take(n)?; self.pos += 1; Ok(line)
    }
    fn u32(&mut self) -> Result<u32, String> { Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| "truncated integer")?)) }
    fn scalar(&mut self, n: usize) -> Result<&'a [u8], String> { if n > MAX_SCALAR { return Err("scalar size limit".into()); } self.take(n) }
    fn check_graph(&self, nodes: usize, edges: usize, scalars: usize) -> Result<(), String> {
        if self.nodes.len().checked_add(nodes).is_none_or(|n| n > self.limits.0) { return Err("graph node limit".into()); }
        if self.edges.checked_add(edges).is_none_or(|n| n > self.limits.1) { return Err("graph edge limit".into()); }
        if self.scalars.checked_add(scalars).is_none_or(|n| n > self.limits.2) { return Err("graph scalar limit".into()); }
        Ok(())
    }
    fn reserve(&mut self, nodes: usize, edges: usize, scalars: usize) -> Result<(), String> {
        self.check_graph(nodes, edges, scalars)?;
        let e = self.edges + edges;
        let s = self.scalars + scalars;
        self.edges = e; self.scalars = s; Ok(())
    }
    fn add(&mut self, node: Node) -> Result<(), String> {
        let edges = match &node { Node::List(v) | Node::Tuple(v) => v.len(), Node::Dict(v) => v.len() * 2, _ => 0 };
        self.reserve(1, edges, 0)?;
        if self.stack.len() >= MAX_ITEMS { return Err("stack limit".into()); }
        let id = self.nodes.len(); self.nodes.push(node); self.push(id)
    }
    fn push(&mut self, id: usize) -> Result<(), String> { if self.stack.len() >= MAX_ITEMS { return Err("stack limit".into()); } self.stack.push(id); Ok(()) }
    fn pop(&mut self) -> Result<usize, String> { self.stack.pop().ok_or_else(|| "stack underflow".into()) }
    fn top(&self) -> Result<usize, String> { self.stack.last().copied().filter(|x| *x != MARK).ok_or_else(|| "invalid stack".into()) }
    fn marked_len(&self) -> Result<usize, String> {
        let pos = self.stack.iter().rposition(|x| *x == MARK).ok_or("missing MARK")?;
        Ok(self.stack.len() - pos - 1)
    }
    fn marked(&mut self) -> Result<Vec<usize>, String> {
        let pos = self.stack.iter().rposition(|x| *x == MARK).ok_or("missing MARK")?;
        let values = self.stack.split_off(pos + 1); self.stack.pop(); Ok(values)
    }
    fn memo_put(&mut self, key: u32) -> Result<(), String> {
        let v = self.top()?; if !self.memo.contains_key(&key) && self.memo.len() >= MAX_MEMO { return Err("memo limit".into()); }
        self.memo.insert(key, v); Ok(())
    }
    fn memo_get(&mut self, key: u32) -> Result<(), String> { let v = *self.memo.get(&key).ok_or("missing memo")?; self.push(v) }
    fn append(&mut self, items: Vec<usize>) -> Result<(), String> {
        let id = self.top()?;
        match self.nodes.get(id) { Some(Node::List(list)) if list.len().checked_add(items.len()).is_some_and(|n| n <= MAX_ITEMS) => {}, _ => return Err("invalid list or container limit".into()) }
        self.reserve(0, items.len(), 0)?;
        if let Node::List(list) = &mut self.nodes[id] { list.extend(items); }
        Ok(())
    }
    fn pairs(&mut self, id: usize, items: Vec<usize>) -> Result<(), String> {
        if items.len() % 2 != 0 { return Err("odd dictionary items".into()); }
        let old = match self.nodes.get(id) { Some(Node::Dict(v)) => v, _ => return Err("invalid dictionary".into()) };
        if old.len().checked_add(items.len()/2).is_none_or(|n| n > MAX_ITEMS) { return Err("container size limit".into()); }
        self.reserve(0, items.len(), 0)?;
        for pair in items.chunks_exact(2) {
            let size = key_size(&self.nodes, pair[0], 0, self.limits.2 - self.scalars)?;
            self.reserve(0, 0, size)?;
            let key = key_repr(&self.nodes, pair[0], 0)?;
            if !self.dict_keys.entry(id).or_default().insert(key) { return Err("duplicate dictionary key".into()); }
        }
        if let Node::Dict(v) = &mut self.nodes[id] { v.extend(items.chunks_exact(2).map(|p| (p[0], p[1]))); }
        Ok(())
    }
}

// Hashable pickle keys have Python equality semantics, including bool/int equality.
// Compute an upper bound before constructing canonical keys, including tuple
// framing. Shared tuple subgraphs may expand, so stop as soon as the budget ends.
fn key_size(nodes: &[Node], id: usize, depth: usize, limit: usize) -> Result<usize, String> {
    if depth > MAX_DEPTH { return Err("key depth limit".into()); }
    let size = match nodes.get(id).ok_or("invalid node")? {
        Node::None | Node::Bool(_) => 2,
        Node::Int(v) => v.len().saturating_add(1),
        Node::Float(_) => 320,
        Node::Text(v) => v.len().saturating_add(32),
        Node::Tuple(v) => {
            let mut n = 1usize;
            for id in v {
                let remaining = limit.checked_sub(n).ok_or("graph scalar limit")?;
                let child = key_size(nodes, *id, depth + 1, remaining)?;
                n = n.checked_add(child).and_then(|x| x.checked_add(32)).ok_or("graph scalar limit")?;
                if n > limit { return Err("graph scalar limit".into()); }
            }
            n
        },
        _ => return Err("unhashable dictionary key".into()),
    };
    if size > limit { return Err("graph scalar limit".into()); }
    Ok(size)
}
fn key_repr(nodes: &[Node], id: usize, depth: usize) -> Result<String, String> {
    if depth > MAX_DEPTH { return Err("key depth limit".into()); }
    match nodes.get(id).ok_or("invalid node")? {
        Node::None => Ok("n".into()), Node::Bool(v) => Ok(format!("i{}", u8::from(*v))),
        Node::Int(v) => Ok(format!("i{v}")),
        Node::Float(v) if *v == 0.0 => Ok("i0".into()),
        Node::Float(v) if v.fract() == 0.0 => Ok(format!("i{:.0}", v)),
        Node::Float(v) => Ok(format!("f{:x}", v.to_bits())),
        Node::Text(v) => Ok(format!("s{}:{v}", v.len())),
        Node::Tuple(v) => { let mut s = String::from("t"); for x in v { let k = key_repr(nodes, *x, depth+1)?; s.push_str(&format!("{}:{k}", k.len())); } Ok(s) },
        _ => Err("unhashable dictionary key".into()),
    }
}
fn text(b: &[u8]) -> Result<&str, String> { std::str::from_utf8(b).map_err(|_| "invalid UTF-8".into()) }
fn integer(s: &str) -> Result<String, String> {
    let (neg, digits) = if let Some(s) = s.strip_prefix('-') { (true, s) } else { (false, s.strip_prefix('+').unwrap_or(s)) };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) { return Err("invalid integer".into()); }
    let d = digits.trim_start_matches('0'); if d.is_empty() { Ok("0".into()) } else if neg { Ok(format!("-{d}")) } else { Ok(d.into()) }
}
fn long_bytes(b: &[u8]) -> String {
    if b.is_empty() { return "0".into(); }
    let negative = b[b.len()-1] & 128 != 0; let mut bytes = b.to_vec();
    if negative { let mut carry = 1u16; for v in &mut bytes { let n = (!*v as u16 & 255) + carry; *v = n as u8; carry = n >> 8; } }
    while bytes.last() == Some(&0) { bytes.pop(); }
    let mut digits = Vec::new(); while !bytes.is_empty() { let mut rem = 0u16; for v in bytes.iter_mut().rev() { let n = rem * 256 + *v as u16; *v = (n / 10) as u8; rem = n % 10; } digits.push((b'0' + rem as u8) as char); while bytes.last() == Some(&0) { bytes.pop(); } }
    if digits.is_empty() { return "0".into(); } if negative { digits.push('-'); } digits.iter().rev().collect()
}
// Protocol zero STRING uses Python byte-string escapes; UNICODE uses raw-unicode-escape.
fn escapes(b: &[u8], unicode: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new(); let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' { if unicode && b[i] >= 128 { let c = char::from(b[i]); let mut buf = [0;4]; out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes()); } else { out.push(b[i]); } i += 1; continue; }
        i += 1; if i == b.len() { return Err("invalid escape".into()); } let c = b[i]; i += 1;
        if unicode && c != b'u' && c != b'U' { out.push(b'\\'); out.push(c); continue; }
        match c {
            b'u' | b'U' if unicode => { let n = if c == b'u' { 4 } else { 8 }; let end = i.checked_add(n).ok_or("escape overflow")?; let s = text(b.get(i..end).ok_or("truncated escape")?)?; let v = u32::from_str_radix(s,16).map_err(|_| "invalid escape")?; let ch = char::from_u32(v).ok_or("invalid unicode")?; let mut buf = [0;4]; out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes()); i = end; },
            b'\\' | b'\'' | b'"' => out.push(c), b'n' => out.push(b'\n'), b'r' => out.push(b'\r'), b't' => out.push(b'\t'), b'a' => out.push(7), b'b' => out.push(8), b'f' => out.push(12), b'v' => out.push(11),
            b'x' => { let end = i+2; let s = text(b.get(i..end).ok_or("truncated escape")?)?; out.push(u8::from_str_radix(s,16).map_err(|_| "invalid escape")?); i=end; },
            b'0'..=b'7' => { let mut v = (c-b'0') as u16; for _ in 0..2 { if i < b.len() && (b'0'..=b'7').contains(&b[i]) { v = v*8 + (b[i]-b'0') as u16; i+=1; } else { break; } } out.push(v as u8); },
            _ => { out.push(b'\\'); out.push(c); }
        }
    }
    if out.len() > MAX_SCALAR { return Err("scalar size limit".into()); } Ok(out)
}

pub fn decode(input: &[u8]) -> Result<Graph, String> {
    decode_with_limits(input, (MAX_GRAPH_NODES, MAX_GRAPH_EDGES, MAX_GRAPH_SCALARS))
}
fn decode_with_limits(input: &[u8], limits: (usize, usize, usize)) -> Result<Graph, String> {
    if input.len() > MAX_BYTES { return Err("pickle byte limit".into()); }
    let mut p = Parser { input, pos: 0, stack: Vec::new(), memo: HashMap::new(), nodes: Vec::new(), dict_keys: HashMap::new(), edges: 0, scalars: 0, limits };
    for count in 0..MAX_OPS {
        let offset = p.pos; let op = p.byte()?;
        match op {
            0x80 => { if offset != 0 || p.byte()? > 2 { return Err("unsupported pickle protocol".into()); } },
            b'.' => { if p.pos != input.len() || p.stack.len() != 1 { return Err("invalid STOP or trailing bytes".into()); } let root = p.pop()?; if root >= p.nodes.len() { return Err("invalid root".into()); } let graph = Graph { root, nodes: p.nodes }; validate(&graph)?; return Ok(graph); },
            b'(' => p.push(MARK)?, b'N' => p.add(Node::None)?, 0x88 => p.add(Node::Bool(true))?, 0x89 => p.add(Node::Bool(false))?,
            b'I' | b'L' => { let line = p.line()?; let s = text(line)?; if op == b'I' && (s == "00" || s == "01") { p.add(Node::Bool(s == "01"))?; } else { let s = if op == b'L' { s.strip_suffix('L').unwrap_or(s) } else { s }; p.reserve(1, 0, s.len().max(1))?; p.add(Node::Int(integer(s)?))?; } },
            b'J' => { let v = p.u32()? as i32; p.reserve(1, 0, 11)?; p.add(Node::Int(v.to_string()))?; }, b'K' => { let v = p.byte()?; p.reserve(1, 0, 3)?; p.add(Node::Int(v.to_string()))?; }, b'M' => { let v = u16::from_le_bytes(p.take(2)?.try_into().map_err(|_| "truncated")?); p.reserve(1, 0, 5)?; p.add(Node::Int(v.to_string()))?; },
            0x8a | 0x8b => { let n = if op == 0x8a { p.byte()? as usize } else { let n = p.u32()?; if n > i32::MAX as u32 { return Err("negative long length".into()); } n as usize }; if n > MAX_BINARY_LONG { return Err("binary integer size limit".into()); } let b = p.scalar(n)?; p.reserve(1, 0, n * 3 + 1)?; let v = long_bytes(b); p.add(Node::Int(v))?; },
            b'F' | b'G' => { let v = if op == b'F' { text(p.line()?)?.parse::<f64>().map_err(|_| "invalid float")? } else { f64::from_be_bytes(p.take(8)?.try_into().map_err(|_| "truncated float")?) }; if !v.is_finite() { return Err("non-finite scalar".into()); } p.add(Node::Float(v))?; },
            b'S' => { let b = p.line()?; if b.len() < 2 || (b[0] != b'\'' && b[0] != b'"') || b[b.len()-1] != b[0] { return Err("invalid string literal".into()); } p.reserve(1, 0, b.len())?; let v = escapes(&b[1..b.len()-1], false)?; if !v.is_ascii() { return Err("non-ASCII STRING".into()); } let s = String::from_utf8(v).map_err(|_| "non-ASCII STRING")?; p.add(Node::Text(s))?; },
            b'V' => { let b = p.line()?; p.reserve(1, 0, b.len() * 2)?; let v = escapes(b, true)?; let s = String::from_utf8(v).map_err(|_| "invalid unicode")?; p.add(Node::Text(s))?; },
            b'T' | b'U' | b'X' => { let n = if op == b'U' { p.byte()? as usize } else { p.u32()? as usize }; let b = p.scalar(n)?; let size = if op == b'X' { n } else { n + b.iter().filter(|&&c| c >= 128).count() }; p.reserve(1, 0, size)?; let s = if op == b'X' { text(b)?.to_owned() } else { b.iter().map(|&c| char::from(c)).collect() }; p.add(Node::Text(s))?; },
            b']' => p.add(Node::List(Vec::new()))?, b')' => p.add(Node::Tuple(Vec::new()))?, b'}' => p.add(Node::Dict(Vec::new()))?,
            b'l' | b't' | b'd' => { let n = p.marked_len()?; p.check_graph(1, n, 0)?; let v = p.marked()?; if op == b'l' { p.add(Node::List(v))?; } else if op == b't' { p.add(Node::Tuple(v))?; } else { p.add(Node::Dict(Vec::new()))?; let id = p.top()?; p.pairs(id, v)?; } },
            0x85..=0x87 => { let n = (op-0x84) as usize; if p.stack.len() < n { return Err("stack underflow".into()); } p.check_graph(1, n, 0)?; let v = p.stack.split_off(p.stack.len()-n); if v.contains(&MARK) || v.contains(&ORDERED) { return Err("invalid tuple".into()); } p.add(Node::Tuple(v))?; },
            b'a' => { p.check_graph(0, 1, 0)?; let v = p.pop()?; p.append(vec![v])?; }, b'e' => { let n = p.marked_len()?; p.check_graph(0, n, 0)?; let v = p.marked()?; p.append(v)?; },
            b's' => { p.check_graph(0, 2, 0)?; let v = p.pop()?; let k = p.pop()?; let id = p.top()?; p.pairs(id, vec![k,v])?; }, b'u' => { let n = p.marked_len()?; p.check_graph(0, n, 0)?; let v = p.marked()?; let id = p.top()?; p.pairs(id,v)?; },
            b'p' | b'q' | b'r' => { let k = if op == b'p' { text(p.line()?)?.parse::<u32>().map_err(|_| "invalid memo index")? } else if op == b'q' { p.byte()? as u32 } else { p.u32()? }; p.memo_put(k)?; },
            b'g' | b'h' | b'j' => { let k = if op == b'g' { text(p.line()?)?.parse::<u32>().map_err(|_| "invalid memo index")? } else if op == b'h' { p.byte()? as u32 } else { p.u32()? }; p.memo_get(k)?; },
            b'c' => { let module = p.line()?; let name = p.line()?; if module != b"collections" || name != b"OrderedDict" { return Err("unsafe GLOBAL".into()); } p.push(ORDERED)?; },
            b'R' => { let args = p.pop()?; let ctor = p.pop()?; if ctor != ORDERED || !matches!(p.nodes.get(args), Some(Node::Tuple(v)) if v.is_empty()) { return Err("unsafe REDUCE".into()); } p.add(Node::Dict(Vec::new()))?; },
            _ => return Err(format!("unsupported opcode 0x{op:02x} at {offset}")),
        }
        if count + 1 == MAX_OPS { return Err("opcode limit".into()); }
    }
    Err("opcode limit".into())
}

fn child(node: &Node, index: usize) -> Option<usize> {
    match node {
        Node::List(v) | Node::Tuple(v) => v.get(index).copied(),
        Node::Dict(v) => v.get(index / 2).map(|(k, v)| if index % 2 == 0 { *k } else { *v }),
        _ => None,
    }
}
fn validate(graph: &Graph) -> Result<(), String> {
    let mut state = vec![0u8; graph.nodes.len()];
    let mut height = vec![0usize; graph.nodes.len()];
    let mut work = vec![(graph.root, 0usize, 0usize)];
    while let Some((id, next, max_height)) = work.last_mut() {
        let node = graph.nodes.get(*id).ok_or("invalid node index")?;
        if state[*id] == 0 {
            match node {
                Node::List(v) | Node::Tuple(v) if v.len() > MAX_ITEMS => return Err("container size limit".into()),
                Node::Dict(v) if v.len() > MAX_ITEMS => return Err("container size limit".into()),
                Node::Float(v) if !v.is_finite() => return Err("non-finite scalar".into()),
                _ => {}
            }
            state[*id] = 1;
        }
        if let Some(c) = child(node, *next) {
            *next += 1;
            let s = *state.get(c).ok_or("invalid node index")?;
            if s == 1 { return Err("cyclic pickle graph".into()); }
            if s == 2 { *max_height = (*max_height).max(height[c]); }
            else {
                if work.len() >= MAX_DEPTH { return Err("container depth limit".into()); }
                work.push((c, 0, 0));
            }
        } else {
            let h = *max_height + 1;
            if h > MAX_DEPTH { return Err("container depth limit".into()); }
            height[*id] = h; state[*id] = 2;
            work.pop();
            if let Some((_, _, parent_height)) = work.last_mut() { *parent_height = (*parent_height).max(h); }
        }
    }
    Ok(())
}
pub fn tagged_json(graph: &Graph) -> Result<Value, String> {
    validate(graph)?;
    fn budget(g: &Graph, id: usize, nodes: &mut usize, bytes: &mut usize) -> Result<(), String> {
        let node = g.nodes.get(id).ok_or("invalid node")?;
        *nodes = nodes.checked_add(1).ok_or("JSON expansion node limit")?;
        if *nodes > MAX_JSON_NODES { return Err("JSON expansion node limit".into()); }
        // Conservative upper bound for serialized JSON, including tags and punctuation.
        let extra = match node {
            Node::Text(v) | Node::Int(v) => v.len().checked_mul(6),
            Node::List(v) | Node::Tuple(v) => v.len().checked_mul(2),
            Node::Dict(v) => v.len().checked_mul(4),
            _ => Some(0),
        }.ok_or("JSON expansion byte limit")?;
        *bytes = bytes.checked_add(64).and_then(|n| n.checked_add(extra)).ok_or("JSON expansion byte limit")?;
        if *bytes > MAX_JSON_BYTES { return Err("JSON expansion byte limit".into()); }
        match node {
            Node::List(v) | Node::Tuple(v) => for id in v { budget(g, *id, nodes, bytes)?; },
            Node::Dict(v) => for (k, v) in v { budget(g, *k, nodes, bytes)?; budget(g, *v, nodes, bytes)?; },
            _ => {}
        }
        Ok(())
    }
    budget(graph, graph.root, &mut 0, &mut 0)?;
    fn convert(g: &Graph, id: usize) -> Result<Value, String> {
        let node = g.nodes.get(id).ok_or("invalid node")?;
        Ok(match node {
            Node::None => Value::Null, Node::Bool(v) => json!(v), Node::Int(v) => if let Ok(n) = v.parse::<i64>() { json!(n) } else if let Ok(n) = v.parse::<u64>() { json!(n) } else { json!({"$int":v}) },
            Node::Float(v) => serde_json::Number::from_f64(*v).map(Value::Number).ok_or("non-finite float")?, Node::Text(v) => json!(v),
            Node::List(v) => Value::Array(v.iter().map(|i| convert(g,*i)).collect::<Result<_,_>>()?), Node::Tuple(v) => json!({"$tuple":v.iter().map(|i| convert(g,*i)).collect::<Result<Vec<_>,_>>()?}),
            Node::Dict(v) => json!({"$dict":v.iter().map(|(k,v)| Ok(vec![convert(g,*k)?,convert(g,*v)?])).collect::<Result<Vec<_>,String>>()?}),
        })
    }
    convert(graph, graph.root)
}
#[cfg(test)]
mod tests;
