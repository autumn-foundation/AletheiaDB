Plan:
1. Delete `Resonator` trait from `src/experimental/temporal/echo.rs` and update `EchoChamber` to use `ActivityDensityResonator` concretely.
   - Remove `pub trait Resonator { ... }`
   - Move `fn resonate(&self, history: &EntityHistory) -> TemporalFingerprint` to `impl ActivityDensityResonator` directly.
   - Update `EchoChamber` to replace `resonator: Box<dyn Resonator>` with `resonator: ActivityDensityResonator`.
   - Update `with_resonator` to accept `ActivityDensityResonator`.
   - Remove any references to `Box<dyn Resonator>`.
2. Wait, what about `EchoChamber` stub tests?
   - Update `EchoChamber`'s `#[cfg(not(feature = "semantic-temporal"))]` block to remove `<R: Resonator + 'static>` from `with_resonator`.
3. Update `.jules/razor.md` with the reduction.
