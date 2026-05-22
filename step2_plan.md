3. **De-Abstract `PropagationModel` in `sybil.rs`**:
   - `PropagationModel` has two impls: `LinearPropagation` and `WildfirePropagation`. Wait, does it have just two?
   - If we want to strictly follow Razor's instructions, we should maybe turn it into an enum instead of a dynamic trait? `sybil::simulate` uses generics `<M: PropagationModel>`. So it's monomorphized, not dynamic dispatch. Is it worth replacing a generic trait with a concrete type if there are two implementations? "Replace single-implementation Traits with concrete Structs." Since there are two implementations (`LinearPropagation` in `sybil.rs` and `WildfirePropagation` in `wildfire.rs`), this trait is NOT a single-implementation trait.

4. **De-Abstract `SemanticRule` in `sentinel.rs`**:
   - `SemanticRule` is implemented by `VectorBanRule` and `NumericRangeRule`. Thus, it's not a single-implementation trait. However, `Sentinel` uses `Box<dyn SemanticRule>`, which introduces dynamic dispatch. Should we convert it to an enum? Razor: "Replace single-implementation Traits with concrete Structs." This is NOT a single-implementation trait.

Wait, are there any other single implementation traits? Let's check `GraphView`? No, it might be multiple. Let's check `StorageObserver`? Multiple. `TemporalVersion`? Multiple. Let's check `OptimizationRule`? Multiple. `QueryState`? Sealed trait pattern.

What about `Resonator`? Yes, only `ActivityDensityResonator`.

Let me check `src/experimental/diagnostics/sentinel.rs` again. `SemanticRule` is used for rules. Is `NumericRangeRule` used? Yes, in `test_sentinel_integration`. What about `VectorBanRule`? Also used.

Let's check `src/experimental/characterization/sybil.rs`. `LinearPropagation` is the only model in `sybil.rs`. `WildfirePropagation` is in `wildfire.rs`.

Let's find if there are other abstractions we can remove.
