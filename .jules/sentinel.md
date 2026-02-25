**[Weak Test Coverage in DeduplicationPolicy]**
**Module:** src/index/temporal.rs
**Summary:** The mutant `delete !` in `EntityTimeline::insert_batch` (DeduplicationPolicy::Reject) survived because existing tests only checked that `Reject` works for duplicates (failure case), but never verified it works for valid data (success case).
**Diagnosis:** WEAK_TEST - Missing positive test case for `DeduplicationPolicy::Reject`.
**Kill Shot:** Added `test_batch_insert_reject_policy_valid_batch` which asserts that `insert_batch` with `Reject` policy succeeds for a batch with unique items. Verified manually that this test fails if the mutant is applied.
