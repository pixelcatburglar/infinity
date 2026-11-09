//! GM account clone-grade changes via the remote slash service.
//! The client forwards slash commands; authority and deadlines are persisted by
//! the subscriptions service. Subscription commerce is not implemented here.
use super::{args::Args, slash_error, CallError, Client};
use infinity_core::messages;
use crate::{services::subscriptions::{management, rules}};

fn parse(args: &[String]) -> Result<(i64, i16, Option<i64>, i64), CallError> {
    let invalid = || slash_error(messages::subscriptions::INVALID_GRADE_REQUEST.into());
    if !(2..=4).contains(&args.len()) { return Err(invalid()); }
    let values = Args::new(args);
    let target = values.i64(0).map_err(|_| invalid())?;
    if target <= 0 { return Err(invalid()); }
    let grade = match values.required(1).map_err(|_| invalid())? {
        "alpha" => rules::ALPHA,
        "omega" => rules::OMEGA,
        _ => return Err(invalid()),
    };
    if grade == rules::ALPHA && args.len() != 2 { return Err(invalid()); }
    let duration = if args.len() >= 3 {
        let value = values.i64(2).map_err(|_| invalid())?;
        if value <= 0 { return Err(invalid()); }
        Some(value)
    } else { None };
    let grace = if args.len() == 4 {
        let value = values.i64(3).map_err(|_| invalid())?;
        if value < 0 { return Err(invalid()); }
        value
    } else { 0 };
    Ok((target, grade, duration, grace))
}

pub(super) async fn run(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    let (target, grade, duration, grace) = parse(args)?;
    let name = match grade {
        rules::ALPHA => "Alpha",
        rules::OMEGA => "Omega",
        _ => unreachable!("parse guarantees a known clone grade"),
    };
    management::set(&c.db, &c.world, c.login.user_id, c.login.role, target, grade, duration, grace).await?;
    Ok(messages::gm::clone_grade_changed(target, name))
}

#[cfg(test)]
mod tests;
