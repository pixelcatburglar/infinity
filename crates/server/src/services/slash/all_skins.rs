//! `/allskins [me|characterID|"complete name"]`: permanently grant the complete
//! ordinary region-visible SKIN set to one character, online or offline. There
//! is no client-side command mirror; `cosmeticsSvc.py` observes the result
//! through its normal license queries and `OnSkinLicenseActivated` notices.
//! Structure, single-use and SKINR entitlements are not granted.
use infinity_core::messages;
use super::{args, slash_error, CallError, Client};

pub(super) async fn command(c: &mut Client, argv: &[String]) -> Result<String, CallError> {
    let character = match argv {
        [] => args::target(c, "me").await?,
        [text] if !text.is_empty() => args::target(c, text).await?,
        _ => return Err(slash_error(messages::gm::GM_ALLSKINS_USAGE.into())),
    };
    let summary = crate::services::ship_cosmetics_mgr::all_skins::grant(&c.db, &c.world, &c.sde, &c.cfg.server.boot_region, character).await?;
    Ok(messages::gm::gm_all_skins_done(character, summary.changed, summary.already_permanent))
}
