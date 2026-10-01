# Acceptance-Criteria Evidence Dossier — Hot-Live Encryption Enable Driver (Issue #3708)

**PR:** #3757 (draft) · **Branch:** `feature/hot-live-encryption-enable` · **HEAD:** `6b1161a`
**Base:** `trunk` (`7bcbd26`) · **Date:** 2026-07-21
**Design:** [`2026-07-20-hot-live-encryption-enable-driver.md`](./2026-07-20-hot-live-encryption-enable-driver.md)

This dossier assembles the effective acceptance-criteria set from **(a)** Issue #3708's
two verbatim body bullets and **(b)** the fuller driver requirements (design doc §6/§7
+ the #3700 reopen-contract), maps each AC to verified code/test evidence, and records the
gate results and known-OK disclosures for the pre-un-draft review.

Line references are against HEAD `6b1161a`. Every named test below was confirmed to exist
via `rg 'fn <name>'` and the whole module was run green (see Gate Results).

---

## A. Issue #3708 — the two verbatim ACs

> - Add runtime keyring-install seams to `IndexPersistenceManager` and `RedbColdStorage` (mirror the PR2 pattern).
> - Enables `encryption enable` to complete hot-live rather than reopen-gated.

The seams themselves (index #3741, cold #3733, WAL #3669) merged **before** this branch;
this PR is the **driver** that consumes all three to satisfy the second bullet. The first
bullet is satisfied transitively (the seams exist and are now wired + exercised end-to-end).

---

## B. Effective AC → Evidence table

Status legend: ✅ verified strong · ⚠️ verified but evidence indirect/partial · ❌ missing

| # | AC (requirement) | Evidence (file:line / test) | Status | Notes |
|---|---|---|---|---|
| AC-1 | **Live plaintext→encrypted transition WITHOUT reopen** | `enable_encryption` (`src/db/encryption_enable.rs:330`); no reopen in body. Test `enable_is_live_no_reopen_required` (`:1698`) asserts on the SAME handle post-enable: `manager.keyring().is_some()`, `wal.is_encrypted()`, a write succeeds, and `persist_indexes()` succeeds (the old fail-closed guard no longer fires). `live_writes_after_enable_persist_encrypted_and_survive_reopen` (`:1026`), `persist_on_live_post_enable_handle_succeeds_and_preserves_aeix`. | ✅ | Strong: proves no-reopen positively (live persist succeeds + AEIX sweep), not merely "reopen not called". |
| AC-2 | **Keyrings installed via all three seams in lock-order-safe sequence** | WAL `install_wal_keyring` (`:467`); index `install_index_keyring` via `install_enable_index_keyring` (`:192`, called in migrate); cold `install_cold_keyring` via `install_enable_cold_keyring` (`:227`). Cold clones the tiered `Arc` under a temporary `historical.read()` then releases before wrap/install (migrate body). | ✅ / ⚠️ (lock-order) | Installs present & ordered. Lock-order **safety** is established by the concurrency review's structural trace (no seam install nests under an ordered hold), **not** a runtime test — see GAP-2. |
| AC-3 | **Wrap passes: index AEIX, cold bare→ACV1, WAL seal** | Index `wrap_enable_index_files` → `mark_index_complete` (`:480`); cold `wrap_plaintext_cold_values` → `mark_cold_complete` (`:511`); WAL seal via `install_wal_keyring` + `retire_enable_plaintext_wal`. Whole-dir sweep tests: `enable_is_live_no_reopen_required`, `restarted_worker_persists_encrypted_after_enable` (`:1738`), `enable_encrypts_vector_index_no_plaintext_survives` (`:2645`), cold `enable_cold_bare_plaintext_roundtrip` (`:1910`). | ✅ | Vector/usearch subtree now covered (`enable_encrypts_vector_index_no_plaintext_survives`), closing the crypto review's LOW #4. |
| AC-4 | **`encryption.state` authority flipped BEFORE clearing the rotation ledger (Option A)** | In `enable_encryption_migrate`: `write_encryption_state_durable(... enabled_with_algorithm ...)` strictly precedes `clear_rotation_state`, with a `#[cfg(test)]` crash-injection seam (`enable_test_hooks::run_after_authority_before_clear`) between them. `enable_crash_after_authority_before_clear_resumes` (`:1516`) reconstructs the durable gap-state; **`authority_flip_precedes_ledger_clear`** now drives the **LIVE** `enable_encryption`, injects a crash at that exact seam, and asserts on-disk (authority=enabled, ledger PRESENT, all layers Complete) then that resume is a clean no-op clearing the ledger and re-wrapping no tier. | ✅ | Now observed on the **live** driver directly (design T3), not only via durable reconstruction — closes GAP-1. Red→green confirmed by neutering the ordering (clear-before-flip) and watching the test fail on "ledger still present". |
| AC-5 | **Crash-resume converges at EVERY interruption point; KCV verified before any wrap/unwrap on every resume path (ledger v3)** | Resume paths `resume_pending_enable` (`:592`) and `resume_pending_enable_cold` (`:711`) both call `verify_resumed_enable_algorithm` **then** `verify_resumed_source_kcv` before any wrap (`:608`/`:619`, `:730`/`:731`). `enable_resume_ciphers` verifies at the earliest touch-point (`rotation.rs:2495`). Matrix tests below. | ✅ | Full matrix has dedicated per-point tests (not collapse-argument only) — see §C. |
| AC-6 | **Algorithm pinned (post-review fix, commits 3744ae2 + 6b1161a)** | Ledger v3 gains `algorithm: Option<Algorithm>` (`rotation.rs:1460`), stamped `Some(algorithm.resolve())` at enable-start (`rotation.rs:1570`). `verify_resumed_enable_algorithm` (`rotation.rs:288`) refuses on mismatch AND fails **closed** when `None` (pre-#3708 ledger, `:311`). Tests `enable_resume_refused_on_algorithm_change` (`:2467`), `enable_resume_wrong_key_refuses_before_wal_seal_no_stray_segment` (`:2559`), `enable_under_concrete_chacha_roundtrips_through_reopen` (`:2203`). | ✅ | Closes crypto review HIGH #1 (split-algorithm brick under `Auto` + cross-CPU/config-edit resume). |
| AC-6b | **KCV/algorithm gate BEFORE the WAL seal+roll side effect (crypto MEDIUM #2)** | `install_pending_enable_wal_keyring` verifies algorithm + KCV **before** building/installing the keyring (before seal/force-roll) (`rotation.rs:2345-2357`). Test `enable_resume_wrong_key_refuses_before_wal_seal_no_stray_segment` (`:2559`) asserts a wrong-key resume refuses with no stray encrypted segment left on disk. | ✅ | Closes crypto review MEDIUM #2 (previously a wrong-key segment was sealed before the later KCV check, bricking even a correct-key retry). |
| AC-7 | **`#[allow(dead_code)]` removed as seams gain callers** | `grep 'allow(dead_code)' src/storage/index_persistence/loader.rs` → none. The index seam's attribute is gone now that `enable_encryption_migrate` is its production caller. | ✅ | Confirmed absent at HEAD. |
| AC-8 | **Double-install treated as idempotent on resume/retry** | `install_enable_index_keyring` (`:192`) / `install_enable_cold_keyring` (`:227`) swallow exactly `*KeyringAlreadyInstalled` → `Ok(())`; `map_index_install_err` / `map_cold_install_err` reclassify only that variant, pass genuine faults through. Tests `double_install_is_idempotent_on_reinvoke` (`:1344`), `map_index_install_err_distinguishes_double_install_from_io_fault` (`:1283`), `map_cold_install_err_...` (`:1312`). | ✅ | Swallow is exactly targeted (seam install is a pure in-memory presence-cell flip whose only Err is the distinguished variant). |
| AC-9 | **No key material logged / written outside `Zeroizing`** | No `info!/debug!/trace!/println!/eprintln!` site in `encryption_enable.rs` touches key/DEK/MEK/KCV/cipher/keyring/secret (grep clean). Crypto review independently confirmed CLEAN across all changed files; the only `panic!` is a test assertion. | ✅ | KCV is non-secret hex but still never logged verbatim by the driver. |
| AC-10 | **Persist thread restarted in-process** | `restart_persistence_worker` (`:541`) called at Step 5 on success (`:428`) and, since 6b1161a, on the **error** path (`:416`) so a failed enable does not leave a permanently-dead worker. Old worker signalled+joined at Step 1 (`:384-388`) before any respawn. Tests `restarted_worker_persists_encrypted_after_enable` (`:1738`), `live_writes_after_enable_persist_encrypted_and_survive_reopen` (`:1026`). | ✅ | Guard-matched to the config spawn condition; no double-writer (join precedes respawn). Error-path restore closes concurrency review LOW #7. |
| AC-11 | **No split-key / split-algorithm state at any crash point** | Every `mark_*_complete` runs strictly AFTER a fully-synchronous idempotent wrap; authority flip is strictly last (Option A); algorithm pinned (AC-6); worker quiesced across the whole wrap window. Crypto review: CLEAN on the same-key/same-algorithm axis; algorithm axis now closed by AC-6. | ✅ | Convergence proven per-interruption-point (§C). |

---

## C. Crash-resume interruption-point matrix — dedicated test per point

Every point named in the design matrix (C0–C6 / P2/P4/P6/P7) has a **dedicated** test that
injects that exact durable state and asserts convergence; none rely on collapse-argument alone.

| Interruption point | Test (`src/db/encryption_enable.rs`) |
|---|---|
| C0 / P0 — before ledger | `enable_crash_before_ledger_reopens_plaintext` (`:1386`) |
| C1 / P1 — after ledger, before WAL roll | `enable_crash_after_ledger_before_wal_resumes` (`:1411`) |
| **P2** — after index wrap, before live install / `mark_index_complete` | `resume_after_index_wrap_before_install` (`:2699`) |
| mid index wrap | `enable_crash_mid_index_wrap_resumes` (`:1819`) |
| C3 / P3 — after WAL roll, before authority flip | `enable_crash_after_wal_before_authority_resumes` (`:1467`) |
| mid WAL retire | `enable_crash_mid_wal_retire_resumes` (`:2132`) |
| **P4** — after cold wrap, before live install / `mark_cold_complete` | `resume_after_cold_wrap_before_install` (`:2762`) |
| mid cold wrap | `enable_crash_mid_cold_wrap_resumes` (`:1970`) |
| P4 limbo — cold Pending, reopen without cold tier | `enable_cold_pending_reopened_without_cold_tier_fails_loudly` (`:2326`) |
| C4 / P5 — in the flip→clear gap (Option-A durable proof) | `enable_crash_after_authority_before_clear_resumes` (`:1516`) |
| C4 / P5 — Option-A ordering observed on the **LIVE** driver (crash injected in the flip→clear window) | `authority_flip_precedes_ledger_clear` (GAP-1) |
| all layers complete, cold, authority still off | `enable_crash_all_layers_complete_cold_authority_off_resumes` (`:2397`) |
| C6 — completed enable, reopen clean | `enable_crash_between_migrate_and_respawn_reopens_clean` (`:1561`) |
| **P6** — source secret changed out-of-band (KCV) | `enable_resume_passphrase_secret_changed_refused_by_kcv` (`:1165`); `enable_resume_wrong_key_refuses_before_wal_seal_no_stray_segment` (`:2559`) |
| **P7** — in-process retry idempotence | `double_install_is_idempotent_on_reinvoke` (`:1344`) |

**Note (design traceability):** the design §8 T8/T10 named these `resume_after_index_wrap_before_install`
/ `resume_after_cold_wrap_before_install` and the correctness review (against the earlier commit
`cbf3c51`) reported them as **not existing** — they were subsequently **added** by the fix commits and
now exist and pass at HEAD, so that LOW review finding is closed.

---

## D. Gaps

| ID | Severity | Status | Gap | Resolution |
|---|---|---|---|---|
| GAP-1 | LOW | ✅ **CLOSED** | **Option-A ordering was not directly observed on the live driver.** It was proven (a) structurally — flip precedes clear — and (b) at the durable-state level by the reconstruction test `enable_crash_after_authority_before_clear_resumes` (`:1516`), which hand-lays (authority=enabled, ledger present). No test caught the **live** `enable_encryption_migrate` in the flip→clear window. | **Implemented `authority_flip_precedes_ledger_clear`.** A `#[cfg(test)]`, thread-local crash-injection seam (`enable_test_hooks::run_after_authority_before_clear`) fires in the EXACT flip→clear window of `enable_encryption_migrate`; the test drives the LIVE `enable_encryption`, injects the crash there, and asserts the on-disk state is exactly (authority=enabled, ledger still present, all layers Complete) — then that a subsequent resume converges to a clean no-op that only clears the ledger (re-wraps no tier), leaving authority=enabled and no ledger. Red→green confirmed by neutering the ordering (clear-before-flip): the test fails on "rotation ledger still present at the flip→clear crash point". The seam is test-only (compiled away in production) and thread-local (one-shot, never perturbs parallel enable tests) — no production-visible behavior change. |
| GAP-2 | LOW | 📝 **DOCUMENTED (structural coverage)** | **No direct lock-order / concurrency assertion test.** Design §8 named T4 `enable_is_lock_order_safe` and T5 `concurrent_create_edge_during_migration`; neither was written. | **Decision: do NOT fabricate a weak runtime test; record as structural-review coverage.** Investigation of the repo's concurrency infrastructure (loom is available; the only loom lock-order model is `tests/havoc_loom_flush_coordinator.rs`, and `src/index/temporal_adjacency.rs:793 test_lock_ordering_deadlock_prevention` is a functional test that itself notes "we can't easily detect deadlocks in a unit test") shows a faithful runtime test is either meaningless or unwriteable: **T4** — the enable path has **no nested lock acquisition to invert** (the loom flush model exists precisely because it *does* have a `writer → sync_handle` nesting); `enable_encryption_migrate` installs each keyring via a leaf-lock-only seam and the cold path clones the tiered `Arc` under a temporary `historical.read()` then RELEASES the guard before wrap/install (`src/db/encryption_enable.rs:501`–`:511`) — no earlier ordered primitive is held across a later one, so a loom model would fabricate a nesting that does not exist and a liveness smoke test proves only liveness, not ordering. **T5** — `enable_encryption(&mut self)` needs an exclusive borrow, so the borrow checker forbids a concurrent `create_edge` on the same handle; a true race cannot be expressed (the cross-handle orphan race is separately covered by Issue #3416's `ValidationFailed` abort). Design §8 T4/T5 rows and note ‡ updated to record this as structural-review coverage with the file:line trace. |

Both LOW gaps are now resolved: GAP-1 is closed by the `authority_flip_precedes_ledger_clear`
live crash-injection test, and GAP-2 is recorded as structural-review coverage (T4/T5 are not
meaningful/writeable as runtime tests — see §D and design §8 note ‡). No BLOCKER / HIGH /
MEDIUM gaps remain. The crypto review's HIGH #1 (algorithm pin) and
MEDIUM #2 (KCV-before-WAL-seal), and the concurrency review's LOW #7 (dead worker on error
return), were all fixed in commits `3744ae2` / `6b1161a` and are verified above (AC-6, AC-6b, AC-10).

---

## E. Gate results (verbatim, run at HEAD `6b1161a`)

```
$ cargo fmt --all --check
FMT_EXIT=0                       # clean, no diffs

$ cargo clippy --lib --features mcp-server -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 51.80s
CLIPPY_EXIT=0                    # clean, zero warnings

$ cargo test --features mcp-server --lib db::encryption_enable
test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 5088 filtered out; finished in 13.46s
TEST_EXIT=0
```

All 35 tests in the `db::encryption_enable` module pass, including every named driver test
and the full crash-resume matrix in §C.

### Ledger format-change sign-off flag

⚠️ **ON-DISK FORMAT CHANGE — REQUIRES SIGN-OFF BEFORE UN-DRAFTING.**
The enable rotation ledger (v3) gains an **additive** `algorithm: Option<Algorithm>` field
(`src/db/rotation.rs:1460`), stamped `Some(algorithm.resolve())` at enable-start. **Legacy
behavior is fail-closed:** a pending-enable ledger written by a pre-#3708 binary has
`algorithm: None`, and `verify_resumed_enable_algorithm` **refuses to resume** such a ledger
(`rotation.rs:311`) rather than risk wrapping remaining files under a possibly-different
algorithm. This is safe (a pre-#3708 in-flight enable is only produced by an older binary
mid-migration) but is a durable-format change that must be signed off before the PR leaves
draft.

### Known-OK disclosures (sandbox, NOT regressions)

- **2 uid-0 havoc fails** — the sandbox runs as uid 0, so chmod-based fault injection is a
  no-op for root: `test_flush_deadlock_on_io_error` (`tests/havoc/havoc_flush_deadlock.rs`)
  and `test_metadata_corruption_on_error` (`tests/regression_flush_corruption.rs`).
- **ENOSPC / link failure on `--all-features`** — the instrumented `--all-features` build
  dies at link (ld Bus error / ENOSPC). Verification baseline is `--features mcp-server`.
- **`--all-features` not run** for the reason above; the gate baseline above uses
  `--features mcp-server`.

---

## F. Commit list (origin/trunk..HEAD)

```
6b1161a fix(encryption): verify pinned algorithm on every enable-resume path; restore persist worker on error (#3708)
3744ae2 fix(encryption): pin resolved AEAD algorithm in enable ledger; gate resume on KCV/algorithm (#3708)
3f71350 style(cypher): collapse nested if in test helper to satisfy clippy
cbf3c51 feat(encryption): hot-live plaintext->encrypted enable driver (#3708)
ec2181e test(encryption): live no-reopen enable behavior tests (#3708)
2a37b2c docs(encryption): design for hot-live encryption enable driver (#3708)
```
