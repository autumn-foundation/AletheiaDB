# Elenchus Journal - Verdicts & Patterns

**[Tanimoto F32 NaN Returns]**
**Module:** `index::vector::hnsw`
**Severity:** 🟡 Suspect
**Finding:** The `usearch` implementation of `MetricKind::Tanimoto` returns `NaN` when comparing identical `F32` vectors (e.g., `[1.0, 0.0]`).
**Evidence:** `test_metric_conversions_accuracy` failed with `NaN` for Tanimoto identity check.
**Recommendation:** Investigate upstream `usearch` behavior or restrict Tanimoto to bitset quantization. For now, Tanimoto verification is excluded from the test suite.

**[Missing Quantization Verification]**
**Module:** `index::vector::hnsw`
**Severity:** 🟡 Suspect
**Finding:** No tests verified that `Quantization` settings (e.g., `I8`) actually affected the index structure or size. Code could silently ignore `quantization` config.
**Evidence:** Original test suite only checked config serialization.
**Recommendation:** Added `test_quantization_reduces_index_size` to verify on-disk size reduction.

**[Metric Conversion Gaps]**
**Module:** `index::vector::hnsw`
**Severity:** 🟡 Suspect
**Finding:** Conversion from usearch distance to AletheiaDB similarity was only tested for Cosine. Other metrics (Euclidean, etc.) rely on unchecked arithmetic assumptions.
**Evidence:** Lack of tests for other metrics.
**Recommendation:** Added `test_metric_conversions_accuracy` covering all supported metrics (except Tanimoto due to NaN issue).
