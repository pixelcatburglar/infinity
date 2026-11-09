//! Live pilot corporation and alliance metadata in the actor-owned ballpark.
//! Destiny slim items use the current session affiliation; no persistent state is changed.
use super::*;

impl System {
    pub(crate) fn prune_combat_character_scopes(&mut self) {
        let Some(facts) = self.combat_facts.as_mut() else { return };
        if facts.scope_cleanup_needed() {
            facts.prune_character_scopes(self.pilots.keys().copied());
        }
    }

    pub(crate) fn refresh_pilot_affiliation(&mut self, character: i64, ship: i64, corp: Option<i64>, alliance: Option<i64>) {
        if self.pilots.get(&character) != Some(&ship) { return; }
        let corp = corp.unwrap_or(-1);
        let alliance = alliance.unwrap_or(-1);
        let Some(ball) = self.balls.get(&ship) else { return };
        if ball.corp_id == corp && ball.alliance_id == alliance { return; }
        self.close_combat_character_boundary(character).expect("combat affiliation boundary invariant");
        let Some(ball) = self.balls.get_mut(&ship) else { return };
        ball.corp_id = corp;
        ball.alliance_id = alliance;
        let slim = ball.slim_item();
        self.rebind_combat_character(character, (corp > 0).then_some(corp), (alliance > 0).then_some(alliance));
        self.broadcast(ship, vec![event(self.current_time, "OnSlimItemChange", vec![Value::Int(ship), slim])]);
    }
}

#[cfg(test)]
mod tests;
