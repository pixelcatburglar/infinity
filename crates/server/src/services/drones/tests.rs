#[cfg(any(feature = "tests-combat", feature = "tests-social", feature = "tests-economy", feature = "tests-effects", feature = "tests-ships", feature = "tests-space", feature = "tests-services", feature = "tests-slash"))] pub(crate) mod fixture;
#[cfg(feature = "tests-combat")] mod launch_combat;
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-ships", feature = "tests-effects"))] pub(in crate::services) mod resources;
#[cfg(feature = "tests-combat")] mod weapon_admission;
#[cfg(feature = "tests-combat")] mod handoff;
#[cfg(feature = "tests-combat")] mod ewar;
