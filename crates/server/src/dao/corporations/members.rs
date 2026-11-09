//! Typed corporation member search; SQL expressions are selected only from server-owned fields.
//! LIKE is server-side: literal contains/prefix/suffix patterns escape SQL wildcards;
//! the explicit LIKE operator accepts SQL `%` and `_` pattern syntax.
use infinity_core::{consts::{FILETIME_SECOND, FILETIME_UNIX_EPOCH, ROLE_DIRECTOR}};
use crate::{db::{self, executor::Executor}};
use tokio_postgres::types::ToSql;

#[derive(Clone)]
pub(crate) enum SearchValue { Number(i64), Text(String) }
#[derive(Clone)]
pub(crate) struct Clause { pub(crate) join: i64, pub(crate) property: &'static str, pub(crate) operator: i64, pub(crate) value: SearchValue }
struct CharacterId { id: i64 }
impl CharacterId { fn from_row(row: &tokio_postgres::Row) -> Self { Self { id: row.get("character_id") } } }

fn pattern(text: &str, op: i64) -> String {
    if op == 10 { return text.to_owned(); }
    let escaped = text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    match op { 9 => format!("%{escaped}%"), 11 => format!("{escaped}%"), 12 => format!("%{escaped}"), _ => escaped }
}

pub(crate) async fn query(ex: &(impl Executor + ?Sized), corp: i64, clauses: &[Clause], implied: bool, titles: bool, valid: [i64; 4]) -> Result<Vec<i64>, db::Error> {
    let mut args: Vec<Box<dyn ToSql + Sync + Send>> = vec![Box::new(corp), Box::new(FILETIME_SECOND), Box::new(FILETIME_UNIX_EPOCH)];
    let mut expression = String::new();
    for clause in clauses {
        let index = args.len() + 1;
        let field = match clause.property {
            "roles" => Some(("corp_roles", "roles")), "rolesAtHQ" => Some(("corp_roles_hq", "roles_hq")),
            "rolesAtBase" => Some(("corp_roles_base", "roles_base")), "rolesAtOther" => Some(("corp_roles_other", "roles_other")),
            "grantableRoles" => Some(("corp_grantable_roles", "grantable_roles")), "grantableRolesAtHQ" => Some(("corp_grantable_hq", "grantable_hq")),
            "grantableRolesAtBase" => Some(("corp_grantable_base", "grantable_base")), "grantableRolesAtOther" => Some(("corp_grantable_other", "grantable_other")),
            _ => None,
        };
        let condition = if let Some((direct, title)) = field {
            let SearchValue::Number(mask) = &clause.value else { unreachable!("validated role operand") };
            args.push(Box::new(*mask));
            let effective = if titles { format!("(ch.{direct} | COALESCE((SELECT bit_or(t.{title}) FROM game.corp_titles t WHERE t.corporation_id=ch.corporation_id AND NOT ch.corp_block_roles AND ch.corp_title_mask & t.title_id <> 0),0))") } else { format!("ch.{direct}") };
            let scope = match clause.property { "roles" | "grantableRoles" => 0, "rolesAtHQ" | "grantableRolesAtHQ" => 1, "rolesAtBase" | "grantableRolesAtBase" => 2, _ => 3 };
            let regular = *mask & valid[scope] & !ROLE_DIRECTOR != 0;
            let ceo_director = scope == 0 && *mask & valid[scope] & ROLE_DIRECTOR != 0;
            let director = if regular { format!("ch.corp_roles & {ROLE_DIRECTOR} <> 0") } else if ceo_director {
                format!("ch.corp_roles & {ROLE_DIRECTOR} <> 0 AND EXISTS (SELECT 1 FROM game.corporations co WHERE co.corporation_id=ch.corporation_id AND NOT co.deleted AND co.ceo_id=ch.character_id)")
            } else { "false".to_owned() };
            let test = if implied { format!("({effective} & ${index}::bigint <> 0 OR ({director}))") } else { format!("({effective} & ${index}::bigint <> 0)") };
            if clause.operator == 8 { format!("NOT {test}") } else { test }
        } else if clause.property == "characterID" {
            let SearchValue::Text(text) = &clause.value else { unreachable!("validated name operand") };
            args.push(Box::new(pattern(text, clause.operator)));
            format!("ch.name ILIKE ${index} ESCAPE '\\'")
        } else {
            let SearchValue::Number(number) = &clause.value else { unreachable!("validated numeric operand") };
            args.push(Box::new(*number));
            if clause.property == "titleMask" {
                let comparison = if clause.operator == 8 { "= 0" } else { "<> 0" };
                format!("(ch.corp_title_mask & ${index}::bigint {comparison})")
            } else {
                let column = if clause.property == "baseID" { "ch.corp_base_id" } else { "COALESCE((SELECT max(e.start_date) FROM game.employment_history e WHERE e.char_id=ch.character_id AND e.corporation_id=ch.corporation_id), (extract(epoch FROM ch.created_at)*$2::bigint)::bigint+$3::bigint)" };
                let op = match clause.operator { 1 => "=", 2 => ">", 3 => ">=", 4 => "<", 5 => "<=", 6 => "<>", _ => unreachable!("validated comparison") };
                format!("({column} {op} ${index}::bigint)")
            }
        };
        expression = if expression.is_empty() { condition } else { format!("({expression} {} {condition})", if clause.join == 1 { "OR" } else { "AND" }) };
    }
    let filter = if expression.is_empty() { String::new() } else { format!(" AND ({expression})") };
    let sql = format!("SELECT ch.character_id FROM game.characters ch WHERE ch.corporation_id=$1 AND NOT ch.deleted AND $2::bigint IS NOT NULL AND $3::bigint IS NOT NULL{filter} ORDER BY ch.character_id");
    let refs: Vec<&(dyn ToSql + Sync)> = args.iter().map(|arg| &**arg as &(dyn ToSql + Sync)).collect();
    Ok(ex.query(&sql, &refs).await?.iter().map(|row| CharacterId::from_row(row).id).collect())
}
