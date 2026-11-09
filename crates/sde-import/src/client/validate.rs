//! Cross-family validation and read-only persistent-reference preflight.
use std::collections::{BTreeMap, BTreeSet};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use crate::report::{manifest::Issue, ReportKey};
use super::{Decoded, FamilyRows};

#[derive(Debug)]
pub struct Validation { pub warnings: Vec<Issue>, pub checked_references: u64 }
pub struct DbPreflight { pub checked_references: u64, pub checked_columns: Vec<String> }

type Index<'a> = BTreeMap<String, BTreeMap<i64, &'a Value>>;
fn integer(value: &Value, context: &str) -> Result<i64> {
    let n = value.as_i64().context(format!("{context}: expected integer"))?;
    if !(0..=i64::from(i32::MAX)).contains(&n) { bail!("{context}: ID outside signed 32-bit domain: {n}"); }
    Ok(n)
}
fn index(rows: &FamilyRows) -> Result<Index<'_>> {
    let mut result = BTreeMap::new();
    for (family, records) in rows {
        if family == "localization_fsd_main" { continue; }
        let mut map = BTreeMap::new();
        for row in records {
            let key = integer(row.get("_key").context(format!("{family}: missing _key"))?, &format!("{family}/_key"))?;
            if !row.is_object() || map.insert(key, row).is_some() { bail!("{family}/{key}: duplicate or malformed record"); }
        }
        result.insert(family.clone(), map);
    }
    Ok(result)
}
fn escape(s: &str) -> String { s.replace('~', "~0").replace('/', "~1") }
struct Checker<'a> { data: Index<'a>, old: Index<'a>, result: Validation }
impl Checker<'_> {
    fn reference(&mut self, family: &str, key: i64, path: &str, value: &Value, target: &str, inherited: bool) -> Result<()> {
        let id = integer(value, &format!("{family}/{key}{path}"))?;
        self.result.checked_references += 1;
        let targets = self.data.get(target).with_context(|| format!("missing family {target}"))?;
        if targets.contains_key(&id) { return Ok(()); }
        if inherited {
            let old_match = self.old.get(family).and_then(|m| m.get(&key)).and_then(|r| r.pointer(path)) == Some(value);
            let old_missing = self.old.get(target).is_some_and(|m| !m.contains_key(&id));
            if old_match && old_missing {
                self.warning("reference", family, key, path, format!("inherited unresolved {target} ID {id}"), json!({"snapshot_key":key,"snapshot_path":path,"target":id}));
                return Ok(());
            }
        }
        bail!("{family}/{key}{path}: unresolved {target} ID {id}")
    }
    fn warning(&mut self, code: &str, family: &str, key: i64, path: &str, message: String, evidence: Value) {
        let resource = super::families::FAMILIES.iter().find(|f| f.name == family).map_or("", |f| f.resource);
        self.result.warnings.push(Issue { severity:"warning".into(), code:code.into(), family:family.into(), key:Some(ReportKey::integer(&key.to_string()).expect("validated integer")), path:path.into(), message, source_relative_path:resource.into(), source_sha256:String::new(), evidence });
    }
    fn field(&mut self, family: &str, key: i64, row: &Value, field: &str, target: &str, required: bool) -> Result<()> {
        let path = format!("/{field}");
        match row.get(field) { Some(v) => self.reference(family,key,&path,v,target,false), None if required => bail!("{family}/{key}{path}: missing required field"), None => Ok(()) }
    }
    fn list(&mut self, family: &str, key: i64, row: &Value, name: &str, field: &str, target: &str) -> Result<()> {
        if let Some(list) = row.get(name) {
            for (i, entry) in list.as_array().with_context(|| format!("{family}/{key}/{name}: expected array"))?.iter().enumerate() {
                let path = format!("/{name}/{i}/{field}");
                self.reference(family,key,&path,entry.get(field).with_context(|| format!("{family}/{key}{path}: missing required field"))?,target,false)?;
            }
        }
        Ok(())
    }
}

pub(super) fn validate(native: &FamilyRows, old_native: &FamilyRows) -> Result<Validation> {
    let mut c = Checker { data:index(native)?, old:index(old_native)?, result:Validation { warnings:Vec::new(), checked_references:0 } };
    for family in super::families::FAMILIES { if !native.contains_key(family.name) { bail!("missing family {}", family.name); } }
    // Clone references, not rows, so validation can mutate the warning accumulator.
    let rows: Vec<_> = c.data.iter().flat_map(|(f, m)| m.iter().map(move |(&k, &r)| (f.clone(), k, r))).collect();
    for (family,key,row) in rows {
        let f = family.as_str();
        for &(field,target,required) in match f {
            "types" => &[("groupID","groups",true),("marketGroupID","marketgroups",false),("wreckTypeID","types",false),("metaGroupID","metagroups",false),("variationParentTypeID","types",false)][..],
            "groups" => &[("categoryID","categories",true)],
            "marketgroups" => &[("parentGroupID","marketgroups",false)],
            "dogmaattributes" => &[("minAttributeID","dogmaattributes",false),("maxAttributeID","dogmaattributes",false),("chargeRechargeTimeID","dogmaattributes",false),("categoryID","dogmaattributecategories",false),("unitID","dogmaunits",false)],
            "dogmaeffects" => &[("durationAttributeID","dogmaattributes",false),("dischargeAttributeID","dogmaattributes",false),("rangeAttributeID","dogmaattributes",false),("falloffAttributeID","dogmaattributes",false),("trackingSpeedAttributeID","dogmaattributes",false),("fittingUsageChanceAttributeID","dogmaattributes",false),("npcActivationChanceAttributeID","dogmaattributes",false),("npcUsageChanceAttributeID","dogmaattributes",false),("resistanceAttributeID","dogmaattributes",false)],
            "blueprints" => &[("blueprintTypeID","types",true)], _ => &[],
        } { c.field(f,key,row,field,target,required)?; }
        if matches!(f,"typedogma"|"typematerials"|"requiredskillsfortypes") { c.reference(f,key,"/_key",&Value::from(key),"types",false)?; }
        if f == "typedogma" { c.list(f,key,row,"dogmaAttributes","attributeID","dogmaattributes")?; c.list(f,key,row,"dogmaEffects","effectID","dogmaeffects")?; }
        if f == "typematerials" { c.list(f,key,row,"materials","materialTypeID","types")?; c.list(f,key,row,"randomizedMaterials","materialTypeID","types")?; }
        if f == "dogmaeffects" { if let Some(v) = row.get("modifierInfo") { for (i,m) in v.as_array().context("modifierInfo must be array")?.iter().enumerate() { for (field,target) in [("modifiedAttributeID","dogmaattributes"),("modifyingAttributeID","dogmaattributes"),("skillTypeID","types"),("groupID","groups"),("effectID","dogmaeffects")] { if let Some(v)=m.get(field) { c.reference(f,key,&format!("/modifierInfo/{i}/{field}"),v,target,false)?; } } } } }
        if f == "requiredskillsfortypes" { for (skill,level) in row.as_object().context("skill row must be object")? { if skill == "_key" { continue; } let id:i64=skill.parse().with_context(|| format!("{f}/{key}/{skill}: invalid skill ID"))?; if id.to_string()!=*skill { bail!("{f}/{key}/{skill}: noncanonical skill ID") } let path=format!("/{}",escape(skill)); c.reference(f,key,&path,&Value::from(id),"types",false)?; if !level.as_u64().is_some_and(|n| n<=5) { bail!("{f}/{key}{path}: invalid required skill level") } } }
        if f == "blueprints" { let activities=row.get("activities").and_then(Value::as_object).with_context(|| format!("{f}/{key}/activities: expected object"))?; for (name,activity) in activities { for section in ["materials","products","skills"] { if let Some(entries)=activity.get(section) { for (i,item) in entries.as_array().with_context(|| format!("{f}/{key}/activities/{name}/{section}: expected array"))?.iter().enumerate() { let path=format!("/activities/{}/{section}/{i}/typeID",escape(name)); c.reference(f,key,&path,item.get("typeID").with_context(|| format!("{f}/{key}{path}: missing"))?,"types",section != "skills")?; if section=="skills" && !item.get("level").and_then(Value::as_u64).is_some_and(|n| n<=5) { bail!("{f}/{key}/activities/{name}/skills/{i}/level: invalid skill level") } } } } } }
        let names: &[(&str,bool)] = match f { "types"=>&[("typeNameID",true),("descriptionID",false)], "groups"=>&[("groupNameID",true)], "categories"=>&[("categoryNameID",true)], "marketgroups"=>&[("nameID",true)], _=>&[] };
        for &(field,required) in names { let path=format!("/{field}"); let Some(v)=row.get(field) else { if required { bail!("{f}/{key}{path}: missing required message ID") } continue }; c.reference(f,key,&path,v,"localization_fsd_en_us",false)?; let id=integer(v,&path)?; c.result.checked_references += 1; if !c.data["localization_fsd_zh"].contains_key(&id) { c.warning("localization",f,key,&path,format!("zh message {id} absent"),json!({"language":"zh","message_id":id,"provenance":"complete supplied localization_fsd_zh keyspace"})); } }
    }
    let markets=&c.data["marketgroups"]; let mut done=BTreeSet::new();
    for &start in markets.keys() { let mut chain=BTreeSet::new(); let mut current=start; while !done.contains(&current) { if !chain.insert(current) { bail!("marketgroups/{current}/parentGroupID: parent cycle") } let Some(parent)=markets.get(&current).and_then(|r| r.get("parentGroupID")) else { break }; current=integer(parent,"marketgroups parentGroupID")?; } done.extend(chain); }
    Ok(c.result)
}

mod database;
pub(super) async fn preflight_database(client: &mut tokio_postgres::Client, decoded: &Decoded) -> Result<DbPreflight> { database::preflight(client,decoded).await }
#[cfg(test)] mod tests;
