//! Live pilot corporation and alliance metadata in the actor-owned ballpark.
//! Destiny slim items use the current session affiliation; no persistent state is changed.
use super::*;

impl System {
    pub(crate) fn refresh_pilot_affiliation(&mut self, character: i64, ship: i64, corp: Option<i64>, alliance: Option<i64>) {
        if self.pilots.get(&character) != Some(&ship) { return; }
        let Some(ball) = self.balls.get_mut(&ship) else { return };
        let corp = corp.unwrap_or(-1);
        let alliance = alliance.unwrap_or(-1);
        if ball.corp_id == corp && ball.alliance_id == alliance { return; }
        ball.corp_id = corp;
        ball.alliance_id = alliance;
        let slim = ball.slim_item();
        self.broadcast(ship, vec![event(self.current_time, "OnSlimItemChange", vec![Value::Int(ship), slim])]);
    }
}

#[cfg(test)]
mod tests;
