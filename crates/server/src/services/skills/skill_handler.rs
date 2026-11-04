//! Bound skillHandler calls used by `eve/client/script/ui/services/skillsvc.py`.
//! Uses game.skills, game.skill_queue, game.characters, game.items and startup
//! SDE data through the training, augmentation and attribute helpers.
//! History, remaps and augmentations are persisted; penalties remain fixed answers.
//! `CharStartTrainingSkill` accepts an accessible unlearned book item and its
//! location (`skillsvc.py:763-767`); learned skills have no item identity here.
//! Direct skill purchases use imported availability and prices without prerequisites.
//! Injector diminishing and rounding are server policy, not a native algorithm.
//! Special crate skill bundle metadata is unavailable and is refused.
use super::*;
use super::attributes::character_attributes;
use super::training::{queue_list, save_queue};
use super::view::skills_dict;
use crate::client::short;
use marshal::Value;
use tracing::warn;

pub(super) fn queue_value(rows: &[crate::dao::skills::queue::QueueEntry]) -> Value {
    let opt = |v: Option<i64>| v.map(Value::Int).unwrap_or(Value::None);
    Value::List(
        rows.iter()
            .map(|r| {
                keyval([
                    ("queuePosition", Value::Int(r.position as i64)),
                    ("trainingTypeID", Value::Int(r.type_id as i64)),
                    ("trainingToLevel", Value::Int(r.to_level as i64)),
                    ("trainingStartSP", Value::Int(r.start_sp)),
                    ("trainingDestinationSP", Value::Int(r.dest_sp)),
                    ("trainingStartTime", opt(r.start_time)),
                    ("trainingEndTime", opt(r.end_time)),
                ])
            })
            .collect(),
    )
}

pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    let me = c.session.int("charid").unwrap_or(0);
    let r = match (call.service, call.method) {
        (SKILL_HANDLER, "GetSkills") | (SKILL_HANDLER, "GetAllSkills") => skills_dict(c, me).await,
        // [(typeID, pointChange)] since the last login
        (SKILL_HANDLER, "GetSkillChangesForISIS") => Ok(Value::list([])),
        (SKILL_HANDLER, "GetSkillBundleInfo") => Err(crate::services::notify_error(crate::messages::crates::METADATA_UNAVAILABLE)),
        (SKILL_HANDLER, "CheckAndSendNotifications") => Ok(Value::None),
        (SKILL_HANDLER, "GetSkillQueueAndFreePoints") => {
            let free: i64 = match crate::dao::characters::attributes::free_points(&c.db, me).await {
                Ok(points) => points,
                Err(e) => return Some(Err(db_err(e))),
            };
            queue_list(&crate::services::identity::Identity::of_client(c), &c.db).await.map(|q| Value::tuple([queue_value(&q), Value::Int(free)]))
        }
        (SKILL_HANDLER, "GetSkillQueue") => queue_list(&crate::services::identity::Identity::of_client(c), &c.db).await.map(|q| queue_value(&q)),
        (SKILL_HANDLER, "SaveNewQueue") => {
            let activate = call.kwargs.get("activate").or_else(|| call.args.get(1)).map(|v| v.as_int() != Some(0)).unwrap_or(true);
            save_queue(&crate::services::identity::Identity::of_client(c), &c.db, &c.sde, call.args.first().unwrap_or(&Value::None), activate, true, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await.map(|()| Value::None)
        }
        (SKILL_HANDLER, "AbortTraining") => match save_queue(&crate::services::identity::Identity::of_client(c), &c.db, &c.sde, &Value::Dict(vec![]), false, false, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await {
            Ok(()) => Ok(Value::None),
            Err(e) => Err(e),
        },
        (SKILL_HANDLER, "GetFreeSkillPoints") => {
            match crate::dao::characters::attributes::free_points(&c.db, me).await {
                Ok(points) => Ok(Value::Int(points)),
                Err(e) => Err(db_err(e)),
            }
        }
        (SKILL_HANDLER, "ApplyFreeSkillPoints") => super::free_points::apply(c, me, call.args).await,
        (SKILL_HANDLER, "GetFreeSkillPointsAppliedToQueue") => super::free_points::preview_queue(c, me).await,
        (SKILL_HANDLER, "GetFreeSkillPointsAppliedToSkills") => super::free_points::preview_skills(c, me, call.args.first().unwrap_or(&Value::List(vec![]))).await,
        (SKILL_HANDLER, "ApplyFreeSkillPointsToQueue") => super::free_points::apply_queue(c, me).await.map(|_| Value::None),
        (SKILL_HANDLER, "ApplyFreeSkillPointsToSkills") => super::free_points::apply_skills(c, me, call.args.first().unwrap_or(&Value::List(vec![]))).await.map(|_| Value::None),
        (SKILL_HANDLER, "CharStartTrainingSkill") => super::books::start_training(c, me, call.args).await,
        (SKILL_HANDLER, "PurchaseSkills") => super::purchase::purchase(c, me, call.args, call.kwargs).await,
        (SKILL_HANDLER, "GetSkillPoints") => match crate::dao::skills::training::total(&c.db, me).await
        {
            Ok(points) => Ok(Value::Int(points)),
            Err(e) => Err(db_err(e)),
        },
        (SKILL_HANDLER, "GetAttributes") => character_attributes(c, me)
            .await
            .map(|a| Value::Dict(a.into_iter().map(|(k, v)| (Value::Int(k), Value::Float(v))).collect())),
        (SKILL_HANDLER, "GetBoosters") => super::brain_reads::get_boosters(c, me).await,
        (SKILL_HANDLER, "GetImplants") => super::brain_reads::get_implants(c, me).await,
        (SKILL_HANDLER, "GetSkillHistory") => super::history::get(c, me, call.kwargs.get("maxresults").or_else(|| call.args.first())).await,
        (SKILL_HANDLER, "GetPenaltySkillPoints") => Ok(Value::Int(0)),
        (SKILL_HANDLER, "GetDiminishedSpFromInjectors") => super::trading::quote(c, me, call.args).await,
        (SKILL_HANDLER, "CheckInjectionConstraints") => super::trading::inject(c, me, call.args, true).await,
        (SKILL_HANDLER, "InjectSkillpoints") => super::trading::inject(c, me, call.args, false).await,
        (SKILL_HANDLER, "SplitSkillInjector") => super::trading::convert(c, me, call.args, true).await,
        (SKILL_HANDLER, "CombineSkillInjector") => super::trading::convert(c, me, call.args, false).await,
        (SKILL_HANDLER, "ExtractSkills") => super::trading::extract(c, me, call.args).await,
        (SKILL_HANDLER, "GetRespecInfo") => super::respec::get(c, me).await,
        (SKILL_HANDLER, "RespecCharacter") => super::respec::apply(c, me, call.args.first().unwrap_or(&Value::None)).await,
        (SKILL_HANDLER, m) => {
            warn!(
                user = c.login.user_id,
                "UNHANDLED skillHandler::{m} args={} kwargs={}",
                short(&marshal::Value::Tuple(call.args.to_vec())),
                short(call.kwargs)
            );
            Ok(Value::None)
        }
        _ => return None,
    };
    Some(r)
}