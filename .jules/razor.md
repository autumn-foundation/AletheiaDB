## [Reduction]
**Bloat:** The `Resonator` trait in `src/experimental/echo.rs` was a single-implementation trait representing speculative generality. `ActivityDensityResonator` was the only implementor, yet the `EchoChamber` API used dynamic dispatch (`Box<dyn Resonator>`) and generics.
**Cut:** Removed the `Resonator` trait entirely. Promoted `ActivityDensityResonator` to be the concrete type used by `EchoChamber` and replaced all `Box<dyn Resonator>` usages with direct instances of `ActivityDensityResonator`.
**Saved:** ~20 lines of code, removed dynamic dispatch overhead, and eliminated mental overhead of tracking a trait that only has one implementation.
