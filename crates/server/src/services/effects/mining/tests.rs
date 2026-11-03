use super::*;

#[test]
fn extraction_controls_cycle_continuation() {
    use crate::lua::events::ModuleCycleOutcome;
    use crate::services::mining::Extraction;
    assert_eq!(extraction_outcome(Extraction { repeat: true, outcome: Some(ModuleCycleOutcome::Completed), error: None }), CycleOutcome::Continue);
    assert_eq!(extraction_outcome(Extraction { repeat: false, outcome: None, error: None }), CycleOutcome::Stop("mining finished"));
    for outcome in [ModuleCycleOutcome::Completed, ModuleCycleOutcome::TargetLost, ModuleCycleOutcome::CargoFull] {
        assert_eq!(extraction_outcome(Extraction { repeat: false, outcome: Some(outcome), error: None }), CycleOutcome::Ended { reason: "mining finished", outcome });
        assert_eq!(extraction_outcome(Extraction { repeat: false, outcome: Some(outcome), error: Some(CallError::Internal(anyhow::anyhow!("reconcile"))) }), CycleOutcome::Ended { reason: "mining failed", outcome });
    }
}
