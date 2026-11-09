//! Source-effective values for an explicitly supplied projected dogma effect.
//! Effect identity and attributes come from the activating source; the fit evaluator
//! consumes these snapshots without loading the source ship. Space owns their
//! transient lifetime and removes them when either participant leaves.

#[derive(Clone, Debug)]
pub(crate) struct Projection {
    pub source_ship: i64,
    pub kind: super::EffectKind,
    pub until_ms: i64,
    pub source_module: i64,
    pub source_type: i32,
    pub effect_id: i32,
    pub values: Vec<(i32, f64)>,
    pub charge_values: Vec<(i32, f64)>,
    pub charge_type: Option<i32>,
    pub scramble: Option<crate::space::combat_facts::scramble::ScrambleHandle>,
}
