//! GM slash commands for effects, mirroring the client's `svc.slash`.
//! `/effects unsupported` lists the module effects pilots tried to activate
//! that the server refused; the data is process-local and the command
//! modifies nothing. `/effect` sends bounded private visual-effect previews
//! of the shared catalogue to the caller's current destiny observer: a bare
//! command, `list`, `help` or `?` prints the catalogue read-only even while
//! docked, while play and stop requests are handed to the running system
//! actor in one `World::with` call. The actor owns every scene check, the
//! sole locked-target fallback, firing-type resolution and the
//! records behind stop; replies only report what was sent, never what the
//! client rendered. Not supported: durations and repeats, broadcasting
//! previews to other observers, and every name the catalogue refuses.

use super::{args, slash_error, CallError, Client};
use infinity_core::messages;
use crate::services::effects::unsupported;
use crate::space::system::gm_effects::{self, Outcome, Refusal, Request};

fn valid_args(args: &[String]) -> bool {
    matches!(args, [action] if action.as_str() == "unsupported")
}

pub(super) fn command(args: &[String]) -> Result<String, CallError> {
    if !valid_args(args) {
        return Err(slash_error(messages::gm::GM_EFFECTS_USAGE.into()));
    }
    Ok(messages::gm::gm_effects_unsupported(&unsupported::snapshot()))
}

/// `/effect`: catalogue reads or a typed request for the system actor.
pub(super) async fn visual_command(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    let Some(first) = args.first() else { return Ok(catalogue()) };
    let rest = &args[1..];
    match first.as_str() {
        "list" | "help" | "?" if rest.is_empty() => Ok(catalogue()),
        "stop-all" if rest.is_empty() => run(c, Request::StopAll).await,
        "stop" | "off" if rest.is_empty() => run(c, Request::StopAll).await,
        "stop" | "off" => stop(c, &rest[0], &rest[1..]).await,
        "list" | "help" | "?" | "stop-all" => Err(usage()),
        _ => play(c, named(first)?, rest).await,
    }
}

/// The usage header plus the full catalogue as it stands in the actor's
/// single table: canonical name, wire GUID, recognized aliases and the typed
/// disposition of each row.
fn catalogue() -> String {
    messages::gm::gm_visual_list(gm_effects::CATALOGUE.iter().map(|entry| {
        let disposition = match entry.disposition {
            Ok(e) if e.targeted() && e.one_shot() => messages::gm::GM_VISUAL_TARGETED_PULSE,
            Ok(e) if e.one_shot() => messages::gm::GM_VISUAL_SELF_PULSE,
            Ok(e) if e.targeted() => messages::gm::GM_VISUAL_TARGETED,
            Ok(_) => messages::gm::GM_VISUAL_SELF,
            Err(refusal) => refusal_text(refusal),
        };
        messages::gm::gm_visual_effect_row(entry.name, entry.guid, &entry.aliases.join(", "), disposition)
    }))
}

fn usage() -> CallError { slash_error(messages::gm::GM_VISUAL_USAGE.into()) }

fn named(name: &str) -> Result<&'static gm_effects::Entry, CallError> {
    gm_effects::find(name).ok_or_else(|| slash_error(messages::gm::gm_visual_unknown(name)))
}

/// A catalogue row admitted for play; refused rows carry their class reason.
fn previewable(entry: &'static gm_effects::Entry) -> Result<gm_effects::VisualEffect, CallError> {
    entry.disposition.map_err(|refusal| slash_error(refusal_text(refusal).into()))
}

fn refusal_text(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::Cloak => messages::gm::GM_VISUAL_EXCLUDED_CLOAK,
        Refusal::Triage => messages::gm::GM_VISUAL_EXCLUDED_TRIAGE,
        Refusal::Travel => messages::gm::GM_VISUAL_EXCLUDED_TRAVEL,
        Refusal::Empty => messages::gm::GM_VISUAL_EXCLUDED_EMPTY,
    }
}

/// Hand one typed request to the running system actor: the caller's current
/// session and observer channel gate it, and an absent actor is
/// `SceneNotReady`, never a cold load.
async fn run(c: &mut Client, request: Request) -> Result<String, CallError> {
    let character = args::positive_character(&c.session)?;
    let (Some(ship), Some(system)) = (c.session.int("shipid").filter(|id| *id > 0), c.session.int("solarsystemid").filter(|id| *id > 0)) else {
        return Err(slash_error(messages::gm::GM_OVERLAY_SPACE_REQUIRED.into()));
    };
    if c.session.int("stationid").is_some_and(|id| id > 0) || c.session.int("structureid").is_some_and(|id| id > 0) {
        return Err(slash_error(messages::gm::GM_OVERLAY_SPACE_REQUIRED.into()));
    }
    let sde = c.sde.clone();
    let client = c.login.client_id;
    let outcome = c.world.with(system, move |s| s.gm_effect(character, client, ship, request, &sde)).await
        .ok_or_else(|| slash_error(messages::gm::GM_VISUAL_SCENE_NOT_READY.into()))?
        .map_err(|error| slash_error(error_text(error)))?;
    Ok(match outcome {
        Outcome::Sent { effect, target, module_type, replaced } =>
            messages::gm::gm_visual_effect_sent(effect.entry().name, target, module_type.map(|m| m.type_id),
                replaced, effect.one_shot(), module_type.is_some_and(|m| m.unchecked), module_type.is_some_and(|m| m.beam_only)),
        Outcome::Stopped { count } => messages::gm::gm_visual_effect_stops(count, matches!(request, Request::StopAll)),
    })
}

async fn play(c: &mut Client, entry: &'static gm_effects::Entry, rest: &[String]) -> Result<String, CallError> {
    let effect = previewable(entry)?;
    let parsed = tail(rest, false)?;
    if parsed.self_named && effect.targeted() { return Err(usage()); }
    if parsed.module_type.is_some() && !effect.needs_module_type() {
        return Err(slash_error(messages::gm::GM_VISUAL_MODULE_UNEXPECTED.into()));
    }
    run(c, Request::Play { effect, target: parsed.target, module_type: parsed.module_type }).await
}

async fn stop(c: &mut Client, name: &str, rest: &[String]) -> Result<String, CallError> {
    let effect = previewable(named(name)?)?;
    let parsed = tail(rest, true)?;
    run(c, Request::Stop { effect, target: parsed.target }).await
}

/// The optional arguments after an effect name: `me` for a self preview, one
/// target as a positional or `target=` positive ball ID, and a positive
/// `moduleType=` type ID. `stop` keeps the target only; duplicates, unknown
/// keys and nonpositive or overflowing integers are syntax errors.
#[derive(Default)]
struct Tail {
    target: Option<i64>,
    module_type: Option<i32>,
    self_named: bool,
}

fn tail(rest: &[String], stop: bool) -> Result<Tail, CallError> {
    let mut parsed = Tail::default();
    for arg in rest {
        if let Some((key, value)) = arg.split_once('=') {
            if key.eq_ignore_ascii_case("target") {
                if parsed.target.is_some() { return Err(usage()); }
                parsed.target = Some(value.parse::<i64>().ok().filter(|id| *id > 0).ok_or_else(usage)?);
            } else if key.eq_ignore_ascii_case("moduletype") && !stop {
                if parsed.module_type.is_some() { return Err(usage()); }
                parsed.module_type = Some(value.parse::<i32>().ok().filter(|id| *id > 0).ok_or_else(usage)?);
            } else {
                return Err(usage());
            }
        } else if arg.eq_ignore_ascii_case("me") && !stop {
            if parsed.self_named { return Err(usage()); }
            parsed.self_named = true;
        } else if let Some(id) = arg.parse::<i64>().ok().filter(|id| *id > 0) {
            if parsed.target.is_some() { return Err(usage()); }
            parsed.target = Some(id);
        } else {
            return Err(usage());
        }
    }
    Ok(parsed)
}

fn error_text(error: gm_effects::Error) -> String {
    match error {
        gm_effects::Error::CallerChanged => messages::gm::GM_VISUAL_CALLER_CHANGED.into(),
        gm_effects::Error::SceneNotReady => messages::gm::GM_VISUAL_SCENE_NOT_READY.into(),
        gm_effects::Error::SourceUnavailable => messages::gm::GM_VISUAL_SOURCE_UNAVAILABLE.into(),
        gm_effects::Error::TargetRequired => messages::gm::GM_VISUAL_TARGET_REQUIRED.into(),
        gm_effects::Error::TargetAmbiguous => messages::gm::GM_VISUAL_TARGET_AMBIGUOUS.into(),
        gm_effects::Error::UnexpectedTarget => messages::gm::GM_VISUAL_UNEXPECTED_TARGET.into(),
        gm_effects::Error::InvalidTarget => messages::gm::GM_VISUAL_INVALID_TARGET.into(),
        gm_effects::Error::TargetMismatch => messages::gm::GM_VISUAL_TARGET_MISMATCH.into(),
        gm_effects::Error::ModuleTypeRequired => messages::gm::GM_VISUAL_MODULE_REQUIRED.into(),
        gm_effects::Error::ModuleTypeAmbiguous(ids) => {
            let visible = &ids[..ids.len().min(super::LIST_ROW_LIMIT)];
            super::finish_list(messages::gm::gm_visual_module_candidates(visible), ids.len() - visible.len())
        }
        gm_effects::Error::ModuleTypeMismatch => messages::gm::GM_VISUAL_MODULE_MISMATCH.into(),
        gm_effects::Error::InvalidModuleType => messages::gm::GM_VISUAL_MODULE_INVALID.into(),
        gm_effects::Error::UnexpectedModuleType => messages::gm::GM_VISUAL_MODULE_UNEXPECTED.into(),
        gm_effects::Error::OneShotCannotStop => messages::gm::GM_VISUAL_ONE_SHOT_STOP.into(),
    }
}

#[cfg(all(test, feature = "tests-slash"))]
mod tests;
