1. **De-Abstract `Resonator` in `echo.rs`**:
   - There is only one implementation: `ActivityDensityResonator`.
   - The `Resonator` trait only defines `fn resonate(&self, history: &EntityHistory) -> TemporalFingerprint;`
   - We will delete the `Resonator` trait.
   - We will move `resonate` to `ActivityDensityResonator`.
   - `EchoChamber` struct will change `resonator: Box<dyn Resonator>` to `resonator: ActivityDensityResonator`.
   - `with_resonator` will take `ActivityDensityResonator` directly instead of `<R: Resonator + 'static>`.

2. **Run tests** and verify everything works.
3. **Commit the changes.**
