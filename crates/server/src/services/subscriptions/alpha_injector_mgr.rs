//! `alphaInjectorMgr` cooldown query for the selected character.
//! `eve/client/script/ui/services/skillsvc.py` and the Alpha injector banner
//! consume a FILETIME deadline. The skills mechanic reads persistent cooldowns;
//! full Alpha restrictions are not modelled here.

use crate::client::Client;
use crate::services::identity::Identity;
use crate::services::{Call, CallResult};
use marshal::Value;

pub(super) async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if (call.service, call.method) != ("alphaInjectorMgr", "GetNextAvailableInjection") {
        return None;
    }
    Some(crate::services::skills::alpha_injections::next(&Identity::of_client(c), &c.db)
        .await.map(Value::Int))
}
