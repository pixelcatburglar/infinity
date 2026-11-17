//! Real database and system-actor regressions for effect families.
#[cfg(feature = "tests-effects")] mod area;
#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash"))] pub(in crate::services) mod fixture;
#[cfg(feature = "tests-effects")] mod heat;
#[cfg(feature = "tests-effects")] mod utilities;
#[cfg(feature = "tests-effects")] mod space;
#[cfg(feature = "tests-effects")] mod salvage;
