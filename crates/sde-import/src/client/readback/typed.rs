//! Independent projections of the columns in db/sde_typed.sql.
use super::{exact, expected_rows, Readback};
use crate::report::{diff::{ExactNumber, ExactValue as V}, manifest::ReadbackDifference, ReportKey};
use anyhow::{anyhow, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use tokio_postgres::{Client, Row};

type Object = BTreeMap<String, V>;
type Rows = BTreeMap<Vec<i32>, BTreeMap<String, Cell>>;
#[derive(Clone, Debug)]
enum Cell { Int(Option<i32>), Big(Option<i64>), Float(Option<f64>), Bool(Option<bool>), Text(Option<String>), Json(Option<V>) }
#[derive(Clone, Copy)]
enum Kind { Int, Big, Float, Bool, Text, Json, Activities }
struct Column { name: &'static str, source: &'static str, kind: Kind }
macro_rules! cols { ($($name:literal => $src:literal : $kind:ident),* $(,)?) => { &[$(Column { name: $name, source: $src, kind: Kind::$kind }),*] }; }
const CATEGORIES: &[Column] = cols!["name"=>"name":Text,"published"=>"published":Bool,"icon_id"=>"iconID":Int,"data"=>"":Json];
const GROUPS: &[Column] = cols!["category_id"=>"categoryID":Int,"name"=>"name":Text,"published"=>"published":Bool,"anchorable"=>"anchorable":Bool,"anchored"=>"anchored":Bool,"fittable_non_singleton"=>"fittableNonSingleton":Bool,"use_base_price"=>"useBasePrice":Bool,"icon_id"=>"iconID":Int,"data"=>"":Json];
const TYPES: &[Column] = cols!["group_id"=>"groupID":Int,"category_id"=>"@category":Int,"name"=>"name":Text,"description"=>"description":Text,"mass"=>"mass":Float,"volume"=>"volume":Float,"capacity"=>"capacity":Float,"radius"=>"radius":Float,"portion_size"=>"portionSize":Int,"base_price"=>"basePrice":Float,"published"=>"published":Bool,"market_group_id"=>"marketGroupID":Int,"race_id"=>"raceID":Int,"graphic_id"=>"graphicID":Int,"icon_id"=>"iconID":Int,"sound_id"=>"soundID":Int,"data"=>"":Json];
const ATTRIBUTES: &[Column] = cols!["name"=>"name":Text,"data_type"=>"dataType":Int,"default_value"=>"defaultValue":Float,"high_is_good"=>"highIsGood":Bool,"stackable"=>"stackable":Bool,"published"=>"published":Bool,"category_id"=>"attributeCategoryID":Int,"unit_id"=>"unitID":Int,"icon_id"=>"iconID":Int,"display_name"=>"displayName":Text,"data"=>"":Json];
const EFFECTS: &[Column] = cols!["name"=>"name":Text,"guid"=>"guid":Text,"effect_category"=>"effectCategoryID":Int,"is_offensive"=>"isOffensive":Bool,"is_assistance"=>"isAssistance":Bool,"is_warp_safe"=>"isWarpSafe":Bool,"disallow_auto_repeat"=>"disallowAutoRepeat":Bool,"published"=>"published":Bool,"electronic_chance"=>"electronicChance":Bool,"propulsion_chance"=>"propulsionChance":Bool,"range_chance"=>"rangeChance":Bool,"duration_attribute_id"=>"durationAttributeID":Int,"discharge_attribute_id"=>"dischargeAttributeID":Int,"range_attribute_id"=>"rangeAttributeID":Int,"falloff_attribute_id"=>"falloffAttributeID":Int,"tracking_speed_attribute_id"=>"trackingSpeedAttributeID":Int,"resistance_attribute_id"=>"resistanceAttributeID":Int,"npc_usage_chance_attribute_id"=>"npcUsageChanceAttributeID":Int,"npc_activation_chance_attribute_id"=>"npcActivationChanceAttributeID":Int,"fitting_usage_chance_attribute_id"=>"fittingUsageChanceAttributeID":Int,"distribution"=>"distribution":Int,"sfx_name"=>"sfxName":Text,"modifier_info"=>"@modifiers":Json,"data"=>"":Json];
const MODIFIERS: &[Column] = cols!["func"=>"func":Text,"domain"=>"domain":Text,"operation"=>"operation":Int,"modified_attribute_id"=>"modifiedAttributeID":Int,"modifying_attribute_id"=>"modifyingAttributeID":Int,"group_id"=>"groupID":Int,"skill_type_id"=>"skillTypeID":Int,"ref_effect_id"=>"effectID":Int];
const MATERIALS: &[Column] = cols!["quantity"=>"quantity":Big];
const TYPE_ATTRS: &[Column] = cols!["value"=>"value":Float];
const TYPE_EFFECTS: &[Column] = cols!["is_default"=>"isDefault":Bool];
const BLUEPRINTS: &[Column] = cols!["max_production_limit"=>"maxProductionLimit":Int,"activities"=>"activities":Activities];
#[derive(Debug)]
pub(crate) struct ProjectionContext { pub family: String, pub key: Option<ReportKey> }
impl std::fmt::Display for ProjectionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "typed projection {}", self.family) }
}
impl std::error::Error for ProjectionContext {}

fn text_integer(v: Option<&V>, required: bool, narrow: bool) -> Result<()> {
    match v {
        None | Some(V::Null) if !required => Ok(()),
        Some(V::Number(n)) => {
            let value = n.as_jsonb_text_i64().map_err(|e| anyhow!(e))?;
            if narrow { i32::try_from(value)?; }
            Ok(())
        }
        _ => Err(anyhow!("expected integer number")),
    }
}
fn activities(v: Option<&V>) -> Result<()> {
    let root = match v { None | Some(V::Null) => return Ok(()), Some(v) => object(v)? };
    for name in ["manufacturing", "research_time", "research_material", "copying", "invention"] {
        let Some(activity) = root.get(name) else { continue };
        let activity = object(activity).with_context(|| format!("/{name}"))?;
        number(activity.get("time")).with_context(|| format!("/{name}/time"))?;
        for part in ["materials", "products", "skills"] {
            let entries = array(activity, part).with_context(|| format!("/{name}/{part}"))?;
            for (index, entry) in entries.iter().enumerate() {
                let entry = object(entry).with_context(|| format!("/{name}/{part}/{index}"))?;
                for (field, required, narrow) in [("typeID", true, true), ("quantity", false, false), ("level", false, true)] {
                    text_integer(entry.get(field), required, narrow).with_context(|| format!("/{name}/{part}/{index}/{field}"))?;
                }
                number(entry.get("probability")).with_context(|| format!("/{name}/{part}/{index}/probability"))?;
            }
        }
    }
    Ok(())
}

fn object(v: &V) -> Result<&Object> { if let V::Object(o) = v { Ok(o) } else { Err(anyhow!("expected object")) } }
fn number(v: Option<&V>) -> Result<Option<f64>> {
    match v {
        Some(V::Number(n)) => {
            let value = n.finite_f64().map_err(|e| anyhow!(e))?;
            if value == 0.0 && !n.is_zero() { return Err(anyhow!("float underflow")); }
            Ok(Some(value))
        }
        None | Some(V::Null) => Ok(None),
        _ => Err(anyhow!("expected number")),
    }
}
fn int(v: Option<&V>) -> Result<Option<i32>> { match v { Some(V::Number(n)) => Ok(Some(n.as_i32_rounded().map_err(|e| anyhow!(e))?)), None | Some(V::Null) => Ok(None), _ => Err(anyhow!("expected number")) } }
fn project(v: Option<&V>, kind: Kind) -> Result<Cell> {
    Ok(match kind {
        Kind::Int => Cell::Int(int(v)?),
        Kind::Big => Cell::Big(match v { Some(V::Number(n)) => Some(n.as_i64_rounded().map_err(|e| anyhow!(e))?), None | Some(V::Null) => None, _ => return Err(anyhow!("expected number")) }),
        Kind::Float => Cell::Float(number(v)?),
        Kind::Bool => Cell::Bool(match v { Some(V::Bool(b)) => Some(*b), Some(V::Number(n)) => Some(!n.is_zero()), _ => None }),
        Kind::Text => Cell::Text(match v { Some(V::String(s)) => Some(s.clone()), Some(V::Object(o)) => match o.get("en") { Some(V::String(s)) => Some(s.clone()), _ => None }, _ => None }),
        Kind::Json => Cell::Json(v.cloned()),
        Kind::Activities => { activities(v)?; Cell::Json(v.cloned()) },
    })
}
fn from_row(row: &Row, col: &Column) -> Result<Cell> {
    Ok(match col.kind {
        Kind::Int => Cell::Int(row.try_get(col.name)?), Kind::Big => Cell::Big(row.try_get(col.name)?),
        Kind::Float => Cell::Float(row.try_get(col.name)?), Kind::Bool => Cell::Bool(row.try_get(col.name)?),
        Kind::Text => Cell::Text(row.try_get(col.name)?), Kind::Json | Kind::Activities => Cell::Json(row.try_get::<_, Option<String>>(col.name)?.map(|s| exact(&s)).transpose()?),
    })
}
fn fields(value: &V, columns: &[Column]) -> Result<BTreeMap<String, Cell>> {
    let o = object(value)?;
    let mut result = BTreeMap::new();
    for c in columns {
        let v = match c.source { "" => Some(value), "@modifiers" => o.get("modifierInfo"), "@category" => None, s => o.get(s) };
        let cell = if c.source == "@modifiers" && v.is_none() { Cell::Json(Some(V::Array(vec![]))) } else { project(v, c.kind).with_context(|| format!("/{}", c.source))? };
        result.insert(c.name.into(), cell);
    }
    Ok(result)
}
fn numeric_key(v: &V) -> Result<i32> { int(Some(v))?.context("non-numeric typed key") }
fn source(decoded: &super::Decoded, family: &str) -> Result<BTreeMap<String, (ReportKey, V)>> {
    expected_rows(family, decoded.compat_raw.get(family).with_context(|| format!("missing compat {family}"))?)
}
fn base(decoded: &super::Decoded, family: &str, columns: &[Column]) -> Result<Rows> {
    let mut rows = Rows::new();
    for (_, (key, v)) in source(decoded, family)? {
        let result: Result<()> = (|| { let id = numeric_key(object(&v)?.get("_key").context("missing _key")?).context("/_key")?;
            if rows.insert(vec![id], fields(&v, columns)?).is_some() { return Err(anyhow!("duplicate typed key /_key")); } Ok(()) })();
        result.with_context(|| ProjectionContext { family: family.into(), key: Some(key.clone()) })?;
    }
    Ok(rows)
}
fn array<'a>(o: &'a Object, key: &str) -> Result<&'a [V]> { match o.get(key) { None => Ok(&[]), Some(V::Array(a)) => Ok(a), _ => Err(anyhow!("{key} is not an array")) } }
fn nested(decoded: &super::Decoded, family: &str, list: &str, id_field: &str, cols: &[Column], index: bool, aggregate: bool) -> Result<Rows> {
    let mut rows = Rows::new();
    for (_, (key, v)) in source(decoded, family)? {
        let result: Result<()> = (|| {
            let o = object(&v)?; let owner = numeric_key(o.get("_key").context("missing _key")?).context("/_key")?;
            for (idx, item) in array(o, list)?.iter().enumerate() {
                let item_obj = object(item).with_context(|| format!("/{list}/{idx}"))?;
                let id = if index { i32::try_from(idx)? } else { numeric_key(item_obj.get(id_field).context("missing nested key")?).with_context(|| format!("/{list}/{idx}/{id_field}"))? };
                let mut f = fields(item, cols).with_context(|| format!("/{list}/{idx}"))?;
                if aggregate { let b = matches!(f.get("is_default"), Some(Cell::Bool(Some(true)))); f.insert("is_default".into(), Cell::Bool(Some(b))); }
                let k = vec![owner, id];
                if let Some(existing) = rows.get_mut(&k) {
                    if !aggregate { return Err(anyhow!("duplicate projected key {k:?} /{list}/{idx}")); }
                    let prior = matches!(existing.get("is_default"), Some(Cell::Bool(Some(true))));
                    let new = matches!(f.get("is_default"), Some(Cell::Bool(Some(true))));
                    existing.insert("is_default".into(), Cell::Bool(Some(prior || new)));
                } else { rows.insert(k, f); }
            }
            Ok(())
        })();
        result.with_context(|| ProjectionContext { family: family.into(), key: Some(key.clone()) })?;
    }
    Ok(rows)
}
fn value(cell: &Cell) -> V {
    match cell {
        Cell::Int(v) => v.map(|v| V::Number(ExactNumber::parse(&v.to_string()).expect("integer"))).unwrap_or(V::Null),
        Cell::Big(v) => v.map(|v| V::Number(ExactNumber::parse(&v.to_string()).expect("integer"))).unwrap_or(V::Null),
        Cell::Float(v) => v.map(|v| if v.is_finite() { V::Number(ExactNumber::parse(&v.to_string()).expect("finite float")) } else { V::String(v.to_string()) }).unwrap_or(V::Null),
        Cell::Bool(v) => v.map(V::Bool).unwrap_or(V::Null), Cell::Text(v) => v.clone().map(V::String).unwrap_or(V::Null), Cell::Json(v) => v.clone().unwrap_or(V::Null),
    }
}
fn equal(a: &Cell, b: &Cell) -> bool { match (a,b) { (Cell::Float(a), Cell::Float(b)) => a.map(f64::to_bits) == b.map(f64::to_bits), (Cell::Json(a), Cell::Json(b)) => a == b, _ => value(a) == value(b) } }
fn report_key(k: &[i32]) -> ReportKey { if k.len() == 1 { ReportKey::integer(&k[0].to_string()).expect("integer key") } else { ReportKey::String(k.iter().map(i32::to_string).collect::<Vec<_>>().join("/")) } }
async fn compare(client: &Client, out: &mut Readback, family: &str, table: &str, keys: &[&str], columns: &[Column], expected: Rows) -> Result<()> {
    let select = keys.iter().map(|s| s.to_string()).chain(columns.iter().map(|c| if matches!(c.kind, Kind::Json | Kind::Activities) { format!("\"{}\"::text AS \"{}\"", c.name, c.name) } else { format!("\"{}\"", c.name) })).collect::<Vec<_>>().join(", ");
    let query = format!("SELECT {select} FROM sde.\"{table}\"");
    let db = client.query(&query, &[]).await?;
    out.families.get_mut(family).expect("family").compared_typed_rows += db.len() as u64;
    let mut actual = Rows::new();
    for row in db {
        let key = keys.iter().map(|k| row.try_get::<_, i32>(*k)).collect::<std::result::Result<Vec<_>, _>>()?;
        let mut fields = BTreeMap::new(); for c in columns { fields.insert(c.name.to_owned(), from_row(&row, c)?); }
        if actual.contains_key(&key) {
            let duplicate = V::Object(fields.iter().map(|(k,v)| (k.clone(), value(v))).collect());
            out.record(family, "typed", table, report_key(&key), None, Some(&duplicate));
        } else { actual.insert(key, fields); }
    }
    let all: BTreeSet<_> = expected.keys().chain(actual.keys()).cloned().collect();
    for key in all {
        let e = expected.get(&key); let a = actual.get(&key); let rk = report_key(&key);
        match (e,a) {
            (Some(e), Some(a)) => for c in columns {
                let left = &e[c.name]; let right = &a[c.name];
                out.families.get_mut(family).unwrap().compared_fields += 1;
                if !equal(left, right) {
                    if let (Cell::Json(left), Cell::Json(right)) = (left, right) {
                        let differences = crate::report::diff::compare(family, rk.clone(), left.as_ref(), right.as_ref(), "", "");
                        out.families.get_mut(family).unwrap().mismatch_count += differences.len() as u64;
                        out.differences.extend(differences.into_iter().map(|d| ReadbackDifference {
                            view: "typed".into(), table: table.into(), key: rk.clone(),
                            path: format!("/{}{}", c.name, d.path), expected_present: d.old_present,
                            actual_present: d.new_present, expected_value: d.old_value, actual_value: d.new_value,
                        }));
                    } else {
                        // Float comparison is bitwise even when decimal JSON equality treats signed zero as equal.
                        out.families.get_mut(family).unwrap().mismatch_count += 1;
                        out.differences.push(ReadbackDifference { view: "typed".into(), table: table.into(), key: rk.clone(), path: format!("/{}", c.name), expected_present: true, actual_present: true, expected_value: Some(value(left)), actual_value: Some(value(right)) });
                    }
                }
            },
            _ => {
                let obj = |r: &BTreeMap<String, Cell>| V::Object(r.iter().map(|(k,v)| (k.clone(), value(v))).collect());
                let left = e.map(obj); let right = a.map(obj);
                out.record(family, "typed", table, rk, left.as_ref(), right.as_ref());
            }
        }
    }
    Ok(())
}
pub(crate) struct TypedExpected(Vec<(&'static str, &'static str, &'static [&'static str], &'static [Column], Rows)>);

pub(crate) fn prepare_typed(decoded: &super::Decoded) -> Result<TypedExpected> {
    let groups = base(decoded, "groups", GROUPS)?;
    let mut types = base(decoded, "types", TYPES)?;
    for row in types.values_mut() {
        let category = match row.get("group_id") { Some(Cell::Int(Some(g))) => groups.get(&vec![*g]).and_then(|r| r.get("category_id")).cloned().unwrap_or(Cell::Int(None)), _ => Cell::Int(None) };
        row.insert("category_id".into(), category);
    }
    Ok(TypedExpected(vec![
        ("categories","inv_categories",&["category_id"],CATEGORIES,base(decoded,"categories",CATEGORIES)?),
        ("groups","inv_groups",&["group_id"],GROUPS,groups),
        ("types","inv_types",&["type_id"],TYPES,types),
        ("typematerials","inv_type_materials",&["type_id","material_type_id"],MATERIALS,nested(decoded,"typematerials","materials","materialTypeID",MATERIALS,false,false)?),
        ("dogmaattributes","dogma_attributes",&["attribute_id"],ATTRIBUTES,base(decoded,"dogmaattributes",ATTRIBUTES)?),
        ("dogmaeffects","dogma_effects",&["effect_id"],EFFECTS,base(decoded,"dogmaeffects",EFFECTS)?),
        ("dogmaeffects","dogma_effect_modifiers",&["effect_id","idx"],MODIFIERS,nested(decoded,"dogmaeffects","modifierInfo","",MODIFIERS,true,false)?),
        ("typedogma","type_attributes",&["type_id","attribute_id"],TYPE_ATTRS,nested(decoded,"typedogma","dogmaAttributes","attributeID",TYPE_ATTRS,false,false)?),
        ("typedogma","type_effects",&["type_id","effect_id"],TYPE_EFFECTS,nested(decoded,"typedogma","dogmaEffects","effectID",TYPE_EFFECTS,false,true)?),
        ("blueprints","blueprints",&["blueprint_type_id"],BLUEPRINTS,base(decoded,"blueprints",BLUEPRINTS)?),
    ]))
}

pub(super) async fn read(client: &Client, expected: TypedExpected, out: &mut Readback) -> Result<()> {
    for (family, table, keys, columns, rows) in expected.0 { compare(client, out, family, table, keys, columns, rows).await?; }
    Ok(())
}

#[cfg(test)]
mod tests;
