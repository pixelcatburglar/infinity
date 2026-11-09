//! Read-only session information and GM security-status controls.
//! Security is stored by Crimewatch and follows its normal notification path.
use super::{slash_error, Client, CallError};
use infinity_core::messages;
use crate::{db::outbox::dispatch_world, services::crimewatch};

fn valid_security(value: f64) -> bool {
    value.is_finite() && (-10.0..=10.0).contains(&value)
}

pub(super) async fn command(c: &mut Client, command: &str, args: &[String]) -> Result<String, CallError> {
    if command == "session" {
        if !args.is_empty() { return Err(slash_error(messages::gm::GM_SESSION_USAGE.into())); }
        let rows = c.session.attributes().map(|(key, value)| (key, format!("{value:?}"))).collect::<Vec<_>>();
        return Ok(messages::gm::gm_session(c.session.sid, &rows));
    }
    if command != "secstatus" {
        return Err(slash_error(messages::gm::GM_INFO_USAGE.into()));
    }
    let usage = || slash_error(messages::gm::GM_SECSTATUS_USAGE.into());
    let parsed = super::args::Args::new(args);
    let (value, target) = match args {
        [] => (None, None),
        [first] => match first.parse::<f64>() {
            Ok(value) => (Some(value), None),
            Err(_) => (None, Some(first.as_str())),
        },
        [_, target] => (Some(parsed.f64(0).map_err(|_| usage())?), Some(target.as_str())),
        _ => return Err(usage()),
    };
    if value.is_some_and(|v| !valid_security(v)) { return Err(usage()); }
    let char_id = match target {
        Some(_) => {
            parsed.required(args.len() - 1).map_err(|_| usage())?;
            parsed.target(c, args.len() - 1).await?
        }
        None => c.session.int("charid").ok_or_else(usage)?,
    };
    let status = if let Some(value) = value {
        let world = &*c.world;
        c.db.transaction(
            |tx, outbox| Box::pin(async move { crimewatch::set_security(tx, outbox, char_id, value).await }),
            |status, outbox| async move { dispatch_world(status, outbox, world) },
        ).await?
    } else {
        crimewatch::security_status(&c.db, char_id).await?
    };
    Ok(messages::gm::gm_secstatus(char_id, status))
}

#[cfg(test)]
mod tests;
