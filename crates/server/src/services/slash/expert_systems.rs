use super::{CallError, Client};

pub(crate) async fn command(c: &mut Client, args: &[String]) -> Result<String, CallError> {
    let (action, id) = match args {
        [action, id] if matches!(action.as_str(), "add" | "remove") => (action.as_str(), id.parse::<i32>().ok()),
        _ => return Err(super::slash_error(crate::messages::gm::GM_EXPERT_SYSTEM_USAGE.into())),
    };
    let type_id = id.ok_or_else(|| super::slash_error(crate::messages::gm::gm_expert_system_type_id_invalid(args.last().map(String::as_str).unwrap_or_default())))?;
    let identity = crate::services::identity::Identity::of_client(c);
    super::super::skills::expert_systems::gm(&identity, &c.db, &c.world, &c.sde, action == "add", type_id,
        |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, Some(identity.char_id))).await
}
