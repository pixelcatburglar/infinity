//! GM inspection and control of durable jobs in `game.jobs` via the slash service.
//! The client forwards remote slash commands; payloads and job editing are unsupported.
use super::{db_err, slash_error, CallError};
use infinity_core::messages;
use crate::{client::filetime_now, dao::jobs, db::{Db, Error}, scheduler};

#[derive(Debug, PartialEq, Eq)]
enum Command<'a> { List, Retry(&'a str), Cancel(&'a str) }

fn parse(args: &[String]) -> Option<Command<'_>> {
    match args {
        [] => Some(Command::List),
        [action, id] if action.as_str() == "retry" && !id.is_empty() => Some(Command::Retry(id.as_str())),
        [action, id] if action.as_str() == "cancel" && !id.is_empty() => Some(Command::Cancel(id.as_str())),
        _ => None,
    }
}

async fn retry(db: &Db, id: &str) -> Result<bool, Error> {
    let id = id.to_owned();
    db.transaction(
        move |tx, _: &mut ()| {
            let id = id.clone();
            Box::pin(async move {
                let Some(mut job) = jobs::lock_job(tx, &id).await? else { return Ok(false) };
                job.due_at = filetime_now();
                scheduler::schedule(tx, &job).await?;
                Ok(true)
            })
        },
        |found, ()| async move {
            if found { scheduler::wake(); }
            Ok(found)
        },
    ).await
}

pub(super) async fn command(db: &Db, args: &[String]) -> Result<String, CallError> {
    match parse(args).ok_or_else(|| slash_error(messages::scheduler::USAGE.into()))? {
        Command::List => {
            let counts = jobs::counts(db).await.map_err(db_err)?;
            let due = jobs::upcoming(db).await.map_err(db_err)?;
            let mut lines = vec![messages::scheduler::COUNTS.to_owned()];
            if counts.is_empty() { lines.push(messages::scheduler::EMPTY.into()); }
            lines.extend(counts.iter().map(|row| messages::scheduler::count(&row.kind, &row.state, row.count)));
            lines.push(messages::scheduler::UPCOMING.into());
            if due.is_empty() { lines.push(messages::scheduler::EMPTY.into()); }
            lines.extend(due.iter().map(|row| messages::scheduler::due(&row.id, &row.kind, &row.state, row.due_at)));
            Ok(lines.join("\n"))
        }
        Command::Retry(id) => {
            if retry(db, id).await.map_err(db_err)? { Ok(messages::scheduler::retried(id)) }
            else { Err(slash_error(messages::scheduler::missing(id))) }
        }
        Command::Cancel(id) => {
            if scheduler::cancel(db, id).await.map_err(db_err)? != 0 {
                scheduler::wake();
                Ok(messages::scheduler::cancelled(id))
            } else { Err(slash_error(messages::scheduler::missing(id))) }
        }
    }
}

#[cfg(test)]
mod tests;
