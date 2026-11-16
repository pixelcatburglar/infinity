//! Extracts the sovereignty development-index tables from the matching client
//! source tree into the deployment dataset. Reads only the whitelisted members
//! `eve/common/script/sys/devIndexUtil.py`, `entosis/occupancyCalculator.py`
//! and `dogma/const.py`, and parses only named literal data: numeric
//! assignments, dicts, lists, tuples and the `enumerate` table literal inside
//! `GetDevIndexLevels`/`GetTimeIndexLevels`. Nothing is executed, imported or
//! evaluated as code; missing, duplicated or non-literal whitelisted symbols
//! fail extraction rather than guessing a value. The dataset is constructed
//! independently from the parsed numbers, not from copied client logic.

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

use super::input;

/// Dataset family name declared in the manifest and stored under `datasets/`.
pub(super) const INDICES_FAMILY: &str = "sovereigntyindices";

/// Client source members read relative to the `--sovereignty-source` root.
const DEV_INDEX_UTIL: &str = "eve/common/script/sys/devIndexUtil.py";
const OCCUPANCY_CALCULATOR: &str = "entosis/occupancyCalculator.py";
const DOGMA_CONST: &str = "dogma/const.py";

/// The dataset format declares exactly six index level bands.
const LEVEL_COUNT: usize = 6;
/// Dogma constants the index tables reference through `const.<name>`.
const ATTRIBUTE_NAMES: [&str; 3] = ["attributeDevIndexMilitary", "attributeDevIndexIndustrial", "attributeDevIndexSovereignty"];
/// `source_resource` naming the fixed extraction input set.
const SOURCE_RESOURCE: &str = "devIndexUtil+occupancyCalculator+dogma.const";
/// Recursion bound for nested literals and symbol expansion.
const MAX_DEPTH: usize = 64;

/// Identity of one external source member, recorded in the published manifest.
pub(super) struct SourceIdentity {
    pub sha256: String,
    pub bytes: u64,
}

/// Extracted dataset bytes plus the provenance of every member read.
pub(super) struct ExtractedIndices {
    pub bytes: Vec<u8>,
    pub sources: BTreeMap<String, SourceIdentity>,
}

/// One literal in a whitelisted statement.
enum Lit {
    Int(i64),
    Float(f64),
    Name(String),
    Attribute(String),
    List(Vec<Lit>),
    Dict(Vec<(Lit, Lit)>),
    Binary(u8, Box<Lit>, Box<Lit>),
}

type Names = BTreeMap<String, Lit>;
type Counts = BTreeMap<String, usize>;
type Constants = BTreeMap<String, i64>;

/// Resolution context for literal evaluation: file-scope symbols plus the
/// dogma constants referenced as `const.<name>`.
struct Scope<'a> {
    counts: &'a Counts,
    names: &'a Names,
    consts: &'a Constants,
}
impl<'a> Scope<'a> {
    fn symbol(&self, name: &str) -> Result<&'a Lit> {
        match self.counts.get(name) {
            None => bail!("missing symbol {name}"),
            Some(count) if *count != 1 => bail!("duplicate symbol assignment: {name}"),
            _ => self.names.get(name).with_context(|| format!("unsupported literal for symbol {name}")),
        }
    }
}

impl Lit {
    fn number(&self, scope: &Scope<'_>, depth: usize) -> Result<f64> {
        ensure!(depth < MAX_DEPTH, "literal expansion exceeds limit");
        match self {
            Lit::Int(v) => Ok(*v as f64),
            Lit::Float(v) => Ok(*v),
            Lit::Name(name) => scope.symbol(name)?.number(scope, depth + 1),
            Lit::Attribute(name) => scope.consts.get(name).with_context(|| format!("missing client constant {name}")).map(|v| *v as f64),
            Lit::Binary(op, left, right) => {
                let (a, b) = (left.number(scope, depth + 1)?, right.number(scope, depth + 1)?);
                let value = match op {
                    b'+' => a + b,
                    b'-' => a - b,
                    b'*' => a * b,
                    b'/' => { ensure!(b != 0.0, "division by zero in literal"); a / b }
                    _ => bail!("unsupported literal operator"),
                };
                ensure!(value.is_finite(), "non-finite literal result");
                Ok(value)
            }
            _ => bail!("expected a numeric literal"),
        }
    }
    /// Integer evaluation keeps i64 precision: literals stay exact, arithmetic
    /// is checked and inexact division is refused rather than truncated.
    fn integer(&self, scope: &Scope<'_>, depth: usize) -> Result<i64> {
        ensure!(depth < MAX_DEPTH, "literal expansion exceeds limit");
        match self {
            Lit::Int(v) => Ok(*v),
            Lit::Float(v) => {
                ensure!(v.fract() == 0.0 && *v >= i64::MIN as f64 && *v < i64::MAX as f64, "expected an integer literal");
                Ok(*v as i64)
            }
            Lit::Name(name) => scope.symbol(name)?.integer(scope, depth + 1),
            Lit::Attribute(name) => scope.consts.get(name).with_context(|| format!("missing client constant {name}")).copied(),
            Lit::Binary(op, left, right) => {
                let (a, b) = (left.integer(scope, depth + 1)?, right.integer(scope, depth + 1)?);
                match op {
                    b'+' => a.checked_add(b),
                    b'-' => a.checked_sub(b),
                    b'*' => a.checked_mul(b),
                    b'/' => { ensure!(b != 0 && a % b == 0, "inexact division in literal"); a.checked_div(b) }
                    _ => bail!("unsupported literal operator"),
                }.context("integer literal out of range")
            }
            _ => bail!("expected an integer literal"),
        }
    }
}

struct Cursor<'a> { text: &'a [u8], pos: usize }
impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<u8> { self.text.get(self.pos).copied() }
    fn take(&mut self, byte: u8) -> Result<()> {
        ensure!(self.peek() == Some(byte), "unexpected literal character");
        self.pos += 1;
        Ok(())
    }
    /// expression := product (('+' | '-') product)*; `depth` bounds nesting.
    fn value(&mut self, depth: usize) -> Result<Lit> {
        ensure!(depth < MAX_DEPTH, "literal nesting exceeds limit");
        let mut lit = self.product(depth)?;
        while matches!(self.peek(), Some(b'+' | b'-')) {
            let op = self.peek().unwrap();
            self.pos += 1;
            lit = Lit::Binary(op, Box::new(lit), Box::new(self.product(depth)?));
        }
        Ok(lit)
    }
    /// product := unary (('*' | '/') unary)*
    fn product(&mut self, depth: usize) -> Result<Lit> {
        let mut lit = self.unary(depth)?;
        while matches!(self.peek(), Some(b'*' | b'/')) {
            let op = self.peek().unwrap();
            self.pos += 1;
            lit = Lit::Binary(op, Box::new(lit), Box::new(self.unary(depth)?));
        }
        Ok(lit)
    }
    fn unary(&mut self, depth: usize) -> Result<Lit> {
        if self.peek() == Some(b'-') {
            self.pos += 1;
            return Ok(Lit::Binary(b'-', Box::new(Lit::Int(0)), Box::new(self.unary(depth + 1)?)));
        }
        self.primary(depth)
    }
    fn primary(&mut self, depth: usize) -> Result<Lit> {
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let first = self.value(depth + 1)?;
                if self.peek() == Some(b',') {
                    let mut items = vec![first];
                    while self.peek() == Some(b',') {
                        self.pos += 1;
                        if self.peek() == Some(b')') { break; }
                        items.push(self.value(depth + 1)?);
                    }
                    self.take(b')')?;
                    return Ok(Lit::List(items));
                }
                self.take(b')')?;
                Ok(first)
            }
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek() != Some(b']') {
                    items.push(self.value(depth + 1)?);
                    if self.peek() == Some(b',') { self.pos += 1; } else { break; }
                }
                self.take(b']')?;
                Ok(Lit::List(items))
            }
            Some(b'{') => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek() != Some(b'}') {
                    let key = self.value(depth + 1)?;
                    self.take(b':')?;
                    items.push((key, self.value(depth + 1)?));
                    if self.peek() == Some(b',') { self.pos += 1; } else { break; }
                }
                self.take(b'}')?;
                Ok(Lit::Dict(items))
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                let start = self.pos;
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == b'.') { self.pos += 1; }
                if matches!(self.peek(), Some(b'e' | b'E')) {
                    self.pos += 1;
                    if matches!(self.peek(), Some(b'+' | b'-')) { self.pos += 1; }
                    while matches!(self.peek(), Some(c) if c.is_ascii_digit()) { self.pos += 1; }
                }
                let token = std::str::from_utf8(&self.text[start..self.pos])?;
                let digits = token.bytes().filter(|b| b.is_ascii_digit()).count();
                let dots = token.bytes().filter(|b| *b == b'.').count();
                ensure!(digits > 0 && dots <= 1, "invalid number literal: {token}");
                if dots == 0 && !token.contains('e') && !token.contains('E') {
                    Ok(Lit::Int(token.parse().context("integer literal out of range")?))
                } else {
                    let value: f64 = token.parse().context("invalid float literal")?;
                    ensure!(value.is_finite(), "non-finite float literal");
                    Ok(Lit::Float(value))
                }
            }
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                let start = self.pos;
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_') { self.pos += 1; }
                let name = std::str::from_utf8(&self.text[start..self.pos])?;
                if self.peek() == Some(b'.') {
                    self.pos += 1;
                    let start = self.pos;
                    while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_') { self.pos += 1; }
                    let member = std::str::from_utf8(&self.text[start..self.pos])?;
                    ensure!(name == "const" && !member.is_empty(), "unsupported attribute access: {name}.{member}");
                    return Ok(Lit::Attribute(member.to_owned()));
                }
                Ok(Lit::Name(name.to_owned()))
            }
            _ => bail!("unsupported literal expression"),
        }
    }
}

/// Parses one whitespace-free literal expression.
fn literal(text: &str) -> Result<Lit> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    ensure!(!compact.is_empty(), "empty literal");
    let mut cursor = Cursor { text: compact.as_bytes(), pos: 0 };
    let lit = cursor.value(0)?;
    ensure!(cursor.pos == compact.len(), "unsupported literal expression");
    Ok(lit)
}

/// One joined statement: comments removed, bracket continuations merged.
struct Statement { indent: usize, text: String }

fn statements(text: &str) -> Result<Vec<Statement>> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut indent = 0usize;
    let mut depth = 0i64;
    for raw in text.lines() {
        let code = raw.split('#').next().unwrap_or("").trim_end();
        if current.is_empty() {
            if code.trim().is_empty() { continue; }
            indent = raw.len() - raw.trim_start().len();
            current = code.trim().to_owned();
        } else {
            current.push(' ');
            current.push_str(code.trim());
        }
        for byte in code.bytes() {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                _ => (),
            }
        }
        ensure!(depth >= 0, "unbalanced brackets in source");
        if depth == 0 { result.push(Statement { indent, text: std::mem::take(&mut current) }); }
    }
    ensure!(depth == 0, "unterminated statement in source");
    Ok(result)
}

fn compact(text: &str) -> String { text.chars().filter(|c| !c.is_whitespace()).collect() }

/// Splits `name = expression` at the first bracket-depth-zero `=`.
fn assignment(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut depth = 0i64;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                if bytes.get(index + 1) == Some(&b'=') || index.checked_sub(1).map(|i| bytes[i]).is_some_and(|b| matches!(b, b'=' | b'!' | b'<' | b'>')) { return None; }
                return Some((text[..index].trim(), text[index + 1..].trim()));
            }
            _ => (),
        }
    }
    None
}

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// File-scope symbol table. Assignment counts and parseable literals are kept
/// apart so an unsupported expression is reported only for a symbol that is
/// actually required, and a duplicate is an error only on lookup.
fn file_scope(body: &[Statement]) -> Result<(Counts, Names)> {
    let mut counts = Counts::new();
    let mut names = Names::new();
    for statement in body.iter().filter(|s| s.indent == 0) {
        let Some((name, rhs)) = assignment(&statement.text) else { continue };
        if !identifier(name) { continue; }
        *counts.entry(name.to_owned()).or_default() += 1;
        if let Ok(lit) = literal(rhs) { names.insert(name.to_owned(), lit); }
    }
    Ok((counts, names))
}

/// The statements of one `def` body, located by its compacted header prefix.
fn function<'a>(body: &'a [Statement], header: &str) -> Result<&'a [Statement]> {
    for (index, statement) in body.iter().enumerate() {
        if statement.indent == 0 && compact(&statement.text).starts_with(header) {
            let end = body[index + 1..].iter().position(|s| s.indent == 0).map_or(body.len(), |p| index + 1 + p);
            return Ok(&body[index + 1..end]);
        }
    }
    bail!("missing function {header}")
}

fn find_assignment<'a>(body: &'a [Statement], name: &str) -> Result<&'a str> {
    let mut found = None;
    for statement in body {
        if let Some((lhs, rhs)) = assignment(&statement.text) {
            if lhs == name {
                ensure!(found.is_none(), "duplicate symbol assignment: {name}");
                found = Some(rhs);
            }
        }
    }
    found.with_context(|| format!("missing symbol {name}"))
}

/// Extracts the balanced argument text of `name(...)` inside a statement.
fn call_inner<'a>(text: &'a str, name: &str) -> Result<&'a str> {
    let start = text.find(name).map(|index| index + name.len()).context("missing call")?;
    let bytes = text.as_bytes();
    ensure!(bytes.get(start) == Some(&b'('), "unsupported call shape");
    let mut depth = 1i64;
    for (index, byte) in bytes[start + 1..].iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            _ => (),
        }
        if depth == 0 { return Ok(&text[start + 1..start + 1 + index]); }
        ensure!(depth > 0, "unbalanced call brackets");
    }
    bail!("unterminated call")
}

/// Splits an argument list at bracket-depth-zero commas.
fn split_args(inner: &str) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut depth = 0i64;
    let mut start = 0usize;
    for (index, byte) in inner.bytes().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => { parts.push(inner[start..index].to_owned()); start = index + 1; }
            _ => (),
        }
    }
    parts.push(inner[start..].to_owned());
    Ok(parts)
}

/// Identifier targets of a `for <targets> in` header; nested identifier
/// tuples flatten so `i, (a, b)` becomes `[i, a, b]`.
fn targets(text: &str) -> Option<Vec<String>> { targets_at(text, 0) }
fn targets_at(text: &str, depth: usize) -> Option<Vec<String>> {
    if depth >= MAX_DEPTH { return None; }
    let mut out = Vec::new();
    let inner = text.trim().strip_prefix('(').and_then(|t| t.strip_suffix(')')).unwrap_or_else(|| text.trim());
    for part in split_args(inner).ok()? {
        let part = part.trim();
        if part.starts_with('(') { out.extend(targets_at(part, depth + 1)?); }
        else if identifier(part) { out.push(part.to_owned()); }
        else { return None; }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// `for <targets> in <iterable>:` split at the bracket-depth-zero ` in `.
fn for_header(text: &str) -> Option<(Vec<String>, &str)> {
    let body = text.strip_prefix("for ")?.strip_suffix(':')?;
    let bytes = body.as_bytes();
    let mut depth = 0i64;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b' ' if depth == 0 && body[index..].starts_with(" in ") => {
                return targets(&body[..index]).map(|targets| (targets, body[index + 4..].trim()));
            }
            _ => (),
        }
    }
    None
}

/// Reads one fixed source member and records its provenance identity.
fn member(root: &Path, name: &str, sources: &mut BTreeMap<String, SourceIdentity>) -> Result<String> {
    let path = root.join(name);
    let bytes = input::read_regular(&path).with_context(|| format!("reading {name}"))?;
    let resolved = path.canonicalize()?;
    ensure!(resolved.starts_with(root), "source member escapes root: {name}");
    sources.insert(name.to_owned(), SourceIdentity { sha256: input::hash(&bytes), bytes: u64::try_from(bytes.len())? });
    String::from_utf8(bytes).with_context(|| format!("source member is not UTF-8: {name}"))
}

struct IndexTables {
    military_attribute: i64,
    industrial_attribute: i64,
    strategic_attribute: i64,
    bands: usize,
    width: f64,
    modifiers: Vec<(f64, f64)>,
    strategic_seconds: Vec<i64>,
}

struct OccupancyTable {
    base: f64,
    min: f64,
    max: f64,
    capital: f64,
    military: Vec<(i64, f64)>,
    industrial: Vec<(i64, f64)>,
    strategic: Vec<(i64, f64)>,
}

fn dogma_const(text: &str) -> Result<Constants> {
    let body = statements(text)?;
    let (counts, names) = file_scope(&body)?;
    let scope = Scope { counts: &counts, names: &names, consts: &Constants::new() };
    let mut consts = Constants::new();
    for name in ATTRIBUTE_NAMES {
        let value = scope.symbol(name)?.integer(&scope, 0)?;
        ensure!(value > 0, "client constant {name} must be positive");
        consts.insert(name.to_owned(), value);
    }
    let mut unique = consts.values().copied().collect::<Vec<_>>();
    unique.sort_unstable();
    unique.dedup();
    ensure!(unique.len() == consts.len(), "client constants must be distinct");
    Ok(consts)
}

/// Whitelists `devIndexUtil.py`: the numeric literals, the band count inside
/// `GetDevIndexLevels`, the modifier `enumerate` table and the day factor
/// inside `GetTimeIndexLevels`. Other statements are never read.
fn dev_index_util(text: &str, consts: &Constants) -> Result<IndexTables> {
    let body = statements(text)?;
    let (counts, names) = file_scope(&body)?;
    let scope = Scope { counts: &counts, names: &names, consts };
    let time_levels = match scope.symbol("timeIndexLevels")? {
        Lit::Dict(items) => {
            let mut table = BTreeMap::new();
            for (key, value) in items {
                let level = key.integer(&scope, 0)?;
                ensure!(table.insert(level, value.integer(&scope, 0)?).is_none(), "duplicate level {level} in timeIndexLevels");
            }
            table
        },
        _ => bail!("timeIndexLevels is not a dict literal"),
    };
    ensure!(time_levels.keys().copied().eq(1..LEVEL_COUNT as i64), "timeIndexLevels keys must be contiguous from 1");
    let decay_rate = scope.symbol("devIndexDecayRate")?.number(&scope, 0)?;
    let decay_time = scope.symbol("devIndexDecayTime")?.number(&scope, 0)?;
    ensure!(decay_rate > 0.0 && decay_time > 0.0, "dev index decay inputs must be positive");
    let width = decay_rate * decay_time;
    ensure!(width.is_finite(), "dev index band width is not finite");

    let levels_fn = function(&body, "defGetDevIndexLevels(")?;
    let total_levels = literal(find_assignment(levels_fn, "totalLevels")?)?.integer(&scope, 0)?;
    ensure!(total_levels > 0, "totalLevels must be positive");
    let bands = usize::try_from(total_levels.checked_add(1).context("totalLevels out of range")?)?;
    let mut enumerable = None;
    for statement in levels_fn {
        let Some((targets, iterable)) = for_header(&statement.text) else { continue };
        if !iterable.starts_with("enumerate(") { continue; }
        ensure!(enumerable.is_none(), "duplicate enumerate table");
        ensure!(targets.len() == 3 && targets[1] == "militaryModifier" && targets[2] == "industrialModifier", "unsupported modifier binding");
        enumerable = Some(iterable);
    }
    let args = split_args(call_inner(enumerable.context("missing modifier table")?, "enumerate")?)?;
    ensure!(args.len() == 1, "unsupported enumerate arguments");
    let Lit::List(pairs) = literal(&args[0])? else { bail!("modifier table is not a list") };
    let mut modifiers = Vec::new();
    for pair in &pairs {
        let Lit::List(items) = pair else { bail!("modifier entry is not a pair") };
        ensure!(items.len() == 2, "modifier entry is not a pair");
        modifiers.push((items[0].number(&scope, 0)?, items[1].number(&scope, 0)?));
    }
    ensure!(modifiers.len() == bands, "modifier count does not match band count");
    ensure!(modifiers.iter().all(|(m, i)| *m > 0.0 && *i > 0.0), "modifiers must be positive");

    let time_fn = function(&body, "defGetTimeIndexLevels(")?;
    let day = literal(find_assignment(time_fn, "day")?)?.integer(&scope, 0)?;
    ensure!(day > 0, "day must be positive");
    let mut strategic_seconds = Vec::new();
    for days in time_levels.values() {
        let seconds = days.checked_mul(day).context("strategic threshold out of range")?;
        ensure!(seconds > 0, "strategic threshold must be positive");
        strategic_seconds.push(seconds);
    }
    ensure!(strategic_seconds.windows(2).all(|w| w[0] < w[1]), "strategic thresholds must increase");
    Ok(IndexTables {
        military_attribute: consts["attributeDevIndexMilitary"],
        industrial_attribute: consts["attributeDevIndexIndustrial"],
        strategic_attribute: consts["attributeDevIndexSovereignty"],
        bands, width, modifiers, strategic_seconds,
    })
}

/// Whitelists `occupancyCalculator.py` top-level numeric assignments and the
/// three per-axis bonus dicts.
fn occupancy(text: &str) -> Result<OccupancyTable> {
    let body = statements(text)?;
    let (counts, names) = file_scope(&body)?;
    let scope = Scope { counts: &counts, names: &names, consts: &Constants::new() };
    let number = |name: &str| -> Result<f64> {
        let value = scope.symbol(name)?.number(&scope, 0)?;
        ensure!(value.is_finite() && value > 0.0, "occupancy value {name} must be finite and positive");
        Ok(value)
    };
    let table = |name: &str| -> Result<Vec<(i64, f64)>> {
        match scope.symbol(name)? {
            Lit::Dict(items) => {
                let mut entries: Vec<(i64, f64)> = Vec::new();
                for (key, value) in items {
                    let level = key.integer(&scope, 0)?;
                    let bonus = value.number(&scope, 0)?;
                    ensure!(bonus.is_finite() && bonus >= 0.0, "occupancy bonus must be finite and nonnegative");
                    ensure!(entries.iter().all(|(l, _)| *l != level), "duplicate level {level} in {name}");
                    entries.push((level, bonus));
                }
                entries.sort_by_key(|(level, _)| *level);
                ensure!(!entries.is_empty(), "{name} must not be empty");
                ensure!(entries.iter().all(|(level, _)| (0..LEVEL_COUNT as i64).contains(level)), "{name} level out of range");
                ensure!(entries.windows(2).all(|w| w[1].0 == w[0].0 + 1), "{name} levels must be contiguous");
                Ok(entries)
            }
            _ => bail!("{name} is not a dict literal"),
        }
    };
    Ok(OccupancyTable {
        base: number("BASE_BONUS")?,
        min: number("MIN_OCCUPANCY_BONUS")?,
        max: number("MAX_OCCUPANCY_BONUS")?,
        capital: number("CAPITAL_BONUS")?,
        military: table("MILITARY_INDEX_TO_BONUS")?,
        industrial: table("INDUSTRIAL_INDEX_TO_BONUS")?,
        strategic: table("STRATEGIC_INDEX_TO_BONUS")?,
    })
}

/// Builds the dataset `data` object and enforces every format invariant on it.
fn dataset(index: &IndexTables, occupancy: &OccupancyTable) -> Result<Value> {
    ensure!(index.bands == LEVEL_COUNT, "unsupported index band count {}", index.bands);
    ensure!(index.strategic_seconds.len() + 1 == index.bands, "strategic threshold count does not match bands");
    let mut floor = 0.0f64;
    let mut military_levels = Vec::new();
    let mut industrial_levels = Vec::new();
    for (i, (military_modifier, industrial_modifier)) in index.modifiers.iter().enumerate() {
        let max = floor + index.width;
        ensure!(max.is_finite() && max > floor, "band bounds must be finite and increasing");
        military_levels.push(json!({"level": i as i64, "min_points": floor, "max_points": max, "modifier": military_modifier}));
        industrial_levels.push(json!({"level": i as i64, "min_points": floor, "max_points": max, "modifier": industrial_modifier}));
        floor = max;
    }
    let bonus_list = |entries: &[(i64, f64)]| entries.iter().map(|(level, bonus)| json!({"level": level, "bonus": bonus})).collect::<Vec<_>>();
    ensure!(occupancy.min > 0.0 && occupancy.min < occupancy.max, "occupancy min must be positive and below max");
    Ok(json!({
        "attribute_ids": {"military": index.military_attribute, "industrial": index.industrial_attribute, "strategic": index.strategic_attribute},
        "military_levels": military_levels,
        "industrial_levels": industrial_levels,
        "strategic_seconds": index.strategic_seconds,
        "occupancy": {"base": occupancy.base, "min": occupancy.min, "max": occupancy.max, "capital": occupancy.capital,
            "military": bonus_list(&occupancy.military), "industrial": bonus_list(&occupancy.industrial), "strategic": bonus_list(&occupancy.strategic)},
    }))
}

/// Extracts the indices dataset from the matching client source tree.
/// `build` is the operator-declared client build identity recorded in the
/// envelope and, for a standalone publication, the bundle directory name.
pub(super) fn extract(root: &Path, build: &str) -> Result<ExtractedIndices> {
    ensure!(input::ordinary(build), "invalid client build");
    let root = root.canonicalize().context("canonicalizing sovereignty source root")?;
    ensure!(root.is_dir(), "sovereignty source root is not a directory");
    let mut sources = BTreeMap::new();
    let consts = dogma_const(&member(&root, DOGMA_CONST, &mut sources)?)?;
    let index = dev_index_util(&member(&root, DEV_INDEX_UTIL, &mut sources)?, &consts)?;
    let occupancy = occupancy(&member(&root, OCCUPANCY_CALCULATOR, &mut sources)?)?;
    let data = dataset(&index, &occupancy)?;
    let envelope = json!({
        "schema": "infinity-content-dataset", "schema_version": 1, "client_build": build,
        "source_resource": SOURCE_RESOURCE,
        "records": [{"key": "", "data": data}],
    });
    let bytes = serde_json::to_vec(&envelope)?;
    ensure!(u64::try_from(bytes.len())? <= input::MAX_BYTES, "dataset exceeds size limit");
    Ok(ExtractedIndices { bytes, sources })
}

#[cfg(test)]
mod tests;
