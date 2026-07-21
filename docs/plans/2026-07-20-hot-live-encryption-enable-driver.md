# Hot-Live Encryption Enable Driver (Issue #3708)

**Status:** Design (implementation in progress)
**Author:** encryption lane
**Date:** 2026-07-20
**Scope:** Turn `AletheiaDB::enable_encryption` from a *reopen-centric* migration
into a *hot-live* one — perform the plaintext → encrypted transition entirely
in-process, with no mandatory `drop + AletheiaDB::open()` reopen, by driving the
three now-merged runtime install seams (WAL #3669, cold #3733, index #3741) plus
an in-process persistence-thread restart.

Secrets discipline: this document names *derivation seams and key versions*
only. No key material, MEK, DEK, KCV value, or ciphertext byte is ever logged,
written, or embedded anywhere. All key handling stays inside `Zeroizing`.

---

## 1. Problem & context

### 1.1 What "reopen-centric" is today

`enable_encryption(&mut self, key_source) -> Result<EnableReport>`
(`src/db/encryption_enable.rs:223`) migrates a durable plaintext database to
encrypted-at-rest in place, crash-consistently, but deliberately returns a
**partial, quiesced** handle and *requires* the caller to drop it and call
`AletheiaDB::open()` again. The loud reopen contract is documented at
`src/db/encryption_enable.rs:160-222`. Concretely, on `Ok`:

- **WAL is encrypted live** — the WAL has an atomic-swap presence cell, so a
  `None → Some` keyring install works in-process
  (`self.wal.install_wal_keyring(keyring)`, body line 303-305).
- **Index / checkpoint / cold persistence is QUIESCED** — the background
  index-persistence thread is signalled and joined as Step 1
  (`persistence_tracker.signal_shutdown()` + `persistence_thread_handle.take()`,
  body line 274-279) and **not restarted**. The stated reason (docs line
  171-182): the `IndexPersistenceManager`'s index keyring "is owned **by value**
  (there is no live `None → Some` install as there is for the WAL)."
- **`persist_indexes` is fail-closed** on the post-enable handle:
  `src/db/admin.rs:70` — `if self.wal.is_encrypted() && manager.keyring().is_none()`
  returns `FailedPrecondition` telling the caller to reopen. The symmetric
  disable guard is `src/db/admin.rs:118`.

The current body (line 227-361) wraps index and cold **files on disk**
(`wrap_enable_index_files`, `wrap_plaintext_cold_values`) but **never** calls
`install_index_keyring` / `install_cold_keyring` on the *live* managers — those
managers keep their plaintext keyring, and the mandatory reopen builds fresh
managers with the keyring via `enable_resume_ciphers`
(`src/db/rotation.rs:2347`, consumed in `config.rs:775`).

### 1.2 Why the live path is needed

The reopen contract's premise — "there is no live `None → Some` index-keyring
install" — is **exactly what Issue #3741 invalidates**.
`install_index_keyring` (`src/storage/index_persistence/loader.rs:164`) *is*
that live install. Once the driver calls it, `manager.keyring()` becomes `Some`,
the `admin.rs:70` fail-closed guard stops firing, and the background persistence
thread can be respawned in-process. The reopen becomes unnecessary; a caller can
keep using the same handle immediately after `enable_encryption` returns.

### 1.3 The three seams and their exact signatures

All three share a shape: a dedicated leaf `install_lock` serializes the body; a
one-way `None → Some` presence check on an atomic-swap cell; a distinguished
`*AlreadyInstalled` `StorageError` on double-install. Rotation is a *separate*
op (`install_*_generation`), never these seams.

**WAL — `src/storage/wal/concurrent_system.rs:885`**
```rust
pub(crate) fn install_wal_keyring(
    &self,
    keyring: crate::encryption::wal_encryption::WalKeyring,
) -> Result<()>
```
- No `#[allow(dead_code)]` — already wired (production consumer = enable engine).
- Runs as the `advance` closure inside the seal→reopen hand-off
  (`seal_active_segment_for_rotation`): drains + fsyncs in-flight ring entries
  into the current plaintext (v13) segment, seals it, stores the keyring into the
  shared presence cell **between** seal and reopen (under the coordinator `writer`
  mutex so no `flush()` interleaves), then opens a fresh encrypted (v16) segment.
- Double-install → `StorageError::WalKeyringAlreadyInstalled { reason }`
  (`concurrent_system.rs:907`).
- Plaintext-segment retire is a *separate* driver step:
  `retire_old_generation_segments(key_version)` via `retire_enable_plaintext_wal`
  (`src/db/rotation.rs:2321`).

**Cold — `src/storage/redb_cold_storage/mod.rs:1239`**
```rust
pub fn install_cold_keyring(&self, keyring: ColdKeyring) -> Result<()>
```
- `pub`, no `#[allow(dead_code)]` — already reachable.
- Bare → ACV1: no seal/reopen. Cold values are individually self-describing
  (`ACV1`); post-install writes wrap, pre-install bare values stay bare.
  Installing alone does **not** retroactively encrypt — wrapping the pre-install
  corpus is the driver's job via `wrap_plaintext_cold_values(&cold_cipher,
  key_version)` (`mod.rs`; advances a durable redb cursor so a crash mid-pass
  resumes with no double-encrypt).
- Double-install → `StorageError::ColdKeyringAlreadyInstalled { reason }`
  (`mod.rs:1248`).
- Disable mirror: `unwrap_encrypted_cold_values`.

**Index — `src/storage/index_persistence/loader.rs:164`**
```rust
#[allow(dead_code)]                       // <-- line 163
pub(crate) fn install_index_keyring(
    &self,
    keyring: IndexKeyring,
) -> crate::core::error::Result<()>
```
- **The only seam still marked `#[allow(dead_code)]` (line 163)** — because its
  driver (this live path) has not landed. The comment (line 159-162) names
  Issue #3708 as the production consumer. **This driver removes the attribute.**
- AEIX re-encrypt: no on-disk seal. The `AEIX` header is per-file and sniffed on
  read, so pre-install plaintext files keep reading and post-install writes are
  AEIX-encrypted. The separate wrap pass is `wrap_enable_index_files`
  (`src/db/rotation.rs:2252`) → `reencrypt::wrap_plaintext_index_dir`.
- Double-install → `StorageError::IndexKeyringAlreadyInstalled { reason }`
  (`loader.rs:177`).
- Companion accessor: `keyring() -> Option<IndexKeyring>` (`loader.rs:131`), a
  lock-free `load_full()` — the one the `admin.rs:70` guard reads.

### 1.4 Error → MCP mapping

All three `*KeyringAlreadyInstalled` variants map to
`(McpErrorCode::FailedPrecondition, retriable=false)`. `map_wal_install_err`
(`src/db/encryption_enable.rs:130`) reclassifies **only**
`WalKeyringAlreadyInstalled` → `Error::FailedPrecondition`; a genuine `WalError`
(I/O / seal fault) passes through unchanged (stays INTERNAL). **The live driver
must add equivalent `map_index_install_err` / `map_cold_install_err` helpers**
(map only the distinguished `Index/ColdKeyringAlreadyInstalled` variants,
pass genuine faults through), and treat those rejections as **idempotence
signals** on the resume/retry path.

### 1.5 The ledger-v3 / KCV substrate (`src/db/rotation.rs`)

The migration is driven off per-field `LayerStatus` (`rotation.rs:1310`:
`Pending | Complete | Skipped`) in a durable `RotationLedger`, **never** off a
positional index or a count of layers. Enable-scope constructor:
`RotationLedger::enable_scope(ENABLE_KEY_VERSION, new_source, index_in_scope,
cold_in_scope)`. `version=3` serializes the full non-secret `KeyProviderConfig`
plus `mek_kcv: Option<String>`.

Key seams the driver uses (all `pub(crate)`):
- `write_enable_ledger(manager, new_source, index_in_scope, cold_in_scope)`
  (`rotation.rs:2165`) — breadcrumb #1; temp→fsync→rename→parent-dir fsync;
  stamps the KCV via `attach_kcv_from_source`.
- `read_enable_ledger` / `EnableLedgerView` with `non_wal_layers_settled()`.
- `build_enable_wal_keyring` (`rotation.rs:2186`),
  `build_enable_index_cipher` (`rotation.rs:2228`),
  `build_enable_cold_cipher` (`rotation.rs:2239`) — MEK → per-layer DEK → cipher.
  `build_enable_index_cipher` is the cipher the live driver wraps in
  `IndexKeyring::single(...)` for `install_index_keyring`.
- `wrap_enable_index_files` (`rotation.rs:2252`), `retire_enable_plaintext_wal`
  (`rotation.rs:2321`).
- `install_pending_enable_wal_keyring` (the pre-replay startup hook).
- `mark_wal_complete`, `mark_index_complete` (flips index + checkpoint),
  `mark_cold_complete`, `mark_wal_retire_complete`.
- `clear_rotation_state` (`rotation.rs:1712`).
- `ENABLE_KEY_VERSION` (`rotation.rs:2072`).
- `enable_resume_ciphers` (`rotation.rs:2347`) + `EnableResumeCiphers` — the
  bundle `open()` builds resume managers/stores under.

**KCV (Issue #3620, Approach C)** — an accidental-change detector (HKDF under
context `"rotation-kcv"` → lowercase hex), *not* a security control (real
integrity = AEAD on the data). The verify function the enable resume calls is
`verify_resumed_source_kcv(source, mek_kcv)` (`rotation.rs:257`) — no-op when
`mek_kcv` is `None`; else loads the MEK from `source` and constant-time compares;
on mismatch → loud error containing `"KCV"` + `"does not match"`, ledger
**RETAINED**. The existing resume already calls it before any wrap
(`encryption_enable.rs:408`).

### 1.6 The `persist_indexes` fail-closed guard the live install unblocks

`src/db/admin.rs:60-79`: the guard exists precisely because, today, a post-enable
handle has an encrypted WAL but a plaintext index keyring, so a persist would
write plaintext index files over the freshly-wrapped `AEIX` snapshot. The guard
condition is `self.wal.is_encrypted() && manager.keyring().is_none()`. Once
Step 4b of the live driver calls `install_index_keyring`, `manager.keyring()`
becomes `Some`, and this window **closes** — persistence (and the restarted
worker) become safe in-process. This is the single seam that makes the reopen
optional.

---

## 2. Brainstorming — candidate mechanisms

A wide, deliberately unfiltered list of ideas considered for the live transition:

**Install ordering**
- B1. Install each tier's keyring immediately *before* its wrap pass.
- B2. Install each tier's keyring immediately *after* its wrap pass.
- B3. Wrap all tiers first, then install all keyrings at the end.
- B4. Install all keyrings first, then wrap all tiers.
- B5. Reuse the *existing* body's per-tier order (WAL → index → WAL-retire →
  cold) and slot each live install adjacent to its existing wrap/mark step.

**Wrap-pass strategy**
- B6. Keep the existing idempotent/cursor-resumable on-disk wrap passes
  unchanged (`wrap_enable_index_files`, `wrap_plaintext_cold_values`); add only
  the live installs.
- B7. Replace on-disk wrap with "install keyring, then rewrite via the manager's
  encrypted write path" (rejected — see §3, would need the manager operational
  mid-migration and re-derive framing).
- B8. Wrap incrementally under the live keyring (stream). (Rejected for v1 —
  the cursor-resumable batch pass is simpler and already crash-proven.)

**Persist-thread restart**
- B9. Respawn the background persistence thread only after authority flip +
  ledger clear (fully migrated), mirroring `config.rs:1418`
  `spawn_background_persistence_thread`.
- B10. Respawn immediately after the index keyring install (earliest safe
  point). (Rejected — leaves a window where cold is still bare but the worker
  could run; keep restart last.)
- B11. Never restart; leave the reopen path as a *fallback* while making it
  optional. (Kept as a compatibility consideration, not the primary path.)

**Idempotence on resume/retry**
- B12. Treat `*KeyringAlreadyInstalled` as "already done, continue" via
  `map_*_install_err` + a match on the distinguished variant.
- B13. Probe `manager.keyring().is_some()` before install and skip. (Weaker than
  B12 — still racy across the leaf lock; keep B12 as the authority, B13 as an
  optional fast-path.)
- B14. Version-stamp the ledger with which live installs completed. (Rejected —
  the on-disk wrap `LayerStatus` already covers durability; live-install state
  is process-local and reconstructed by the seam's own presence cell.)

**Signalling / structure**
- B15. Add `map_index_install_err` / `map_cold_install_err` mirroring
  `map_wal_install_err`.
- B16. Update the loud reopen-contract doc block + `admin.rs` guard commentary to
  reflect that reopen is no longer mandatory.
- B17. Keep the CLI (`aletheia encryption enable`) exiting-then-reopening; it is
  a fresh process, so it always goes through `enable_resume_ciphers` and never
  double-installs. The live path benefits the embedded/`&mut self` API caller.

---

## 3. Reverse brainstorming — how could this driver break things?

For each failure mode, the inverted safeguard.

| # | Failure mode ("how to corrupt / lose keys / deadlock / split-key") | Inverted safeguard |
|---|---|---|
| R1 | **Plaintext written over AEIX**: restart the persist worker while the index keyring is still plaintext | Restart the worker ONLY after `install_index_keyring` makes `manager.keyring()` `Some`; the `admin.rs:70` guard remains as belt-and-suspenders |
| R2 | **Split-key authority**: flip `encryption.state` to `enabled` while a layer (cold) is still bare | Option-A binding order — flip authority ONLY after every layer's wrap + install + `mark_*_complete`; a crash before the flip resumes plaintext-readable |
| R3 | **Lost key on wrong secret**: a resume re-derives a *different* MEK (secret changed out-of-band) and unwraps garbage | `verify_resumed_source_kcv` runs BEFORE any wrap/unwrap on every resume path; mismatch aborts loud, ledger retained |
| R4 | **Double-encrypt**: a retry re-wraps an already-AEIX/ACV1 corpus | Wrap passes are idempotent (skip AEIX) and cursor-resumable (cold advances a durable redb cursor); `*KeyringAlreadyInstalled` treated as idempotent |
| R5 | **Deadlock**: install a seam while holding `historical`/`wal`/`current_timestamp` | Never hold an ordered write primitive across a seam install; take `historical.read()` only to fetch the tiered `Arc`, release before wrap/install (§7) |
| R6 | **Deadlock inside WAL store closure**: closure calls back into cold flush / `historical` | The WAL store closure mutates only the in-memory presence cell — the seam already forbids acquiring anything ordered after `wal` |
| R7 | **Half-installed in-process retry**: WAL installed, index not, re-invoke → hard error on WAL | `map_wal_install_err` + `map_index/cold_install_err` map `*AlreadyInstalled` → FailedPrecondition, and the driver treats them as continue-signals |
| R8 | **Cold limbo**: crash after cold on-disk wrap, reopen with no cold tier | `fail_if_pending_enable_cold_without_tier` (`encryption_enable.rs:557`) fails LOUD — no silent limbo |
| R9 | **Data loss on WAL retire**: retire plaintext WAL before the encrypted snapshot durably holds pre-enable state | Keep the existing ordering — Step 1b synchronous plaintext persist + index wrap happen BEFORE `retire_enable_plaintext_wal` |
| R10 | **Key material leak**: log a DEK/MEK/KCV or write it outside `Zeroizing` | No key bytes are ever logged/written; ciphers live behind `Arc<dyn Cipher>`, keys inside `Zeroizing`; KCV is non-secret hex but still never logged verbatim by the driver |
| R11 | **Torn keyring read**: a concurrent reader sees a half-installed cell | The seam's atomic-swap cell + leaf `install_lock` guarantee `None → Some` atomicity (loader.rs tests `_no_torn_reads_on_cell`) |
| R12 | **Authority says enabled, WAL replay can't decrypt on next open** | `install_pending_enable_wal_keyring` pre-replay hook installs the WAL keyring before replay reads |

---

## 4. Six Thinking Hats

**White (facts / what the code guarantees).** The WAL already installs live via
an atomic-swap cell (`install_wal_keyring`, no `#[allow(dead_code)]`). Cold and
index seams exist and are tested (`loader.rs:449-654`,
`redb_cold_storage` tests). The index seam alone still carries
`#[allow(dead_code)]` (`loader.rs:163`). The `admin.rs:70` guard fires iff
`wal.is_encrypted() && manager.keyring().is_none()`. The existing body order is
WAL install → index wrap → WAL retire → cold wrap → flip authority → clear
ledger (`encryption_enable.rs:299-352`). Resume already KCV-verifies before wrap
(`:408`) and defers on unsettled non-WAL layers (`non_wal_layers_settled()`).

**Red (intuition — riskiest parts).** The scariest step is restarting the
persist worker: a single mis-ordered respawn writes plaintext over ciphertext.
Second scariest is the cold tier — it has no seal, so the *authority flip* is the
only thing standing between "bare values under an enabled authority" and
correctness; getting Option-A ordering wrong there is silent corruption.

**Black (critical failure modes & why dangerous).** (a) Split-key: authority
`enabled` over a bare/plaintext layer reads as encrypted but decrypt fails →
data appears lost. (b) Deadlock: a seam install nested under `historical`/`wal`
hold violates lock order → hangs the whole DB. (c) Double-encrypt on a
non-idempotent retry → unrecoverable. (d) Wrong-secret resume unwrapping garbage
→ silent corruption. Each is catastrophic and unrecoverable without the
safeguards in §3.

**Yellow (benefits).** No mandatory reopen: an embedded `&mut self` caller
enables encryption and keeps serving from the same handle — no downtime window,
no cold-start index reload, no lost in-RAM warm state. The `admin.rs:70`
fail-closed window closes the instant the index keyring installs, so
`persist_indexes` and the background worker "just work" post-enable.

**Green (creative alternatives).** (i) Wrap-all-then-install-all (§5-B). (ii)
A per-tier staged ledger that also records live-install completion (rejected —
the seam presence cell already reconstructs that). (iii) Lazy worker respawn on
first write instead of eagerly at end of enable (deferred; eager is simpler and
matches `config.rs:1418`).

**Blue (process / sequencing discipline).** The whole migration is driven off
per-field `LayerStatus`, never a count. Lock order is
`current_timestamp → wal → historical → …`; no seam install nests under an
ordered hold. Binding order is Option A: wrap every plaintext byte, install every
live keyring, `mark_*_complete` each, THEN flip authority, THEN clear ledger,
THEN restart the worker. Every resume path KCV-verifies first and is idempotent.

---

## 5. Implementation approaches & tradeoffs

Three candidate sequencings for the wrap + live-install work.

**Approach A — install-then-wrap, per tier.** For each tier: install the live
keyring first, then run the wrap pass (which now writes through the encrypted
manager).

**Approach B — wrap-all-then-install-all.** Run all on-disk wrap passes (WAL
seal, index AEIX, cold ACV1) to completion first, then install all three live
keyrings in a batch, then flip authority.

**Approach C — hybrid staged, per-tier, ledger-tracked (the existing body order
+ a live install slotted after each wrap, before its `mark_*_complete`).** For
each tier in the current lock-safe order: wrap on disk → install live keyring →
`mark_*_complete`. WAL keeps its existing install-then-retire shape; index and
cold gain a live install between wrap and mark.

| Criterion | A (install→wrap) | B (wrap-all→install-all) | C (hybrid staged) |
|---|---|---|---|
| Correctness under crash | Weak — installing before wrap means the manager's encrypted write path must be operational mid-migration; a crash between install and wrap leaves an installed-but-unwrapped tier with no durable `LayerStatus` distinguishing it | Good — each tier's on-disk `LayerStatus` is authoritative; live installs are process-local and replayed by the seam cell | **Best** — matches the crash-proven existing order; each wrap has its own breadcrumb; live install is a process-local addendum |
| Lock-order safety | Same install-site constraints as C, but more interleaving to audit | Installs batched at the end — one audit site, but the batch still must not nest under `historical`/`wal` | **Best** — reuses the existing body's proven release-before-install discipline (§7) |
| Idempotence on resume | Harder — install-before-wrap has no clean "already wrapped, skip" signal | Good | **Best** — resume reuses `enable_resume_ciphers` (fresh `Some` managers, no live install needed); in-process retry hits `*AlreadyInstalled` = continue |
| Complexity | High — needs the manager operational under the new keyring mid-migration | Medium — a second batch pass over tiers | **Low** — smallest diff from today's body |
| Blast radius | Large — reorders the whole migration | Medium — new batch phase | **Small** — adds 2 installs + 1 worker respawn + doc/guard edits |

Rejected: **A** (install-before-wrap forces the manager operational mid-migration
and loses the clean idempotent-skip signal). **B** is viable but adds a phase and
a second pass with no crash-consistency benefit over C.

---

## 6. Chosen approach & rationale

**Approach C — hybrid staged, per-tier, ledger-tracked.**

It is the minimal, lowest-blast-radius change that satisfies every hard
constraint:

- **Lock-order-safe install sequence** — reuses the existing body's discipline:
  WAL install runs in its own `wal`-class seal hand-off; index wrap + install are
  leaf-lock only; cold takes `historical.read()` only to fetch the tiered `Arc`,
  releases, then wraps + installs (§7). No seam install nests under an ordered
  hold.
- **Option-A authority-flip-before-ledger-clear** — unchanged from today
  (`encryption_enable.rs:341-352`): flip `encryption.state` to `enabled` only
  after every wrap + install + `mark_*_complete`, then `clear_rotation_state`.
- **KCV verify on every resume path** — unchanged; `verify_resumed_source_kcv`
  already runs before any wrap (`:408`); resume rebuilds fresh `Some` managers
  via `enable_resume_ciphers`, so no live install is needed on the resume path.
- **Double-install = idempotence** — new `map_index_install_err` /
  `map_cold_install_err` treat `*KeyringAlreadyInstalled` as continue; an
  in-process retry that already installed a seam converges without double-wrap.
- **No split-key state at any crash point** — because the live install is a
  process-local addendum to a per-tier durable `LayerStatus` breadcrumb, and the
  authority flip is strictly last, every crash lands in a state that is either
  fully plaintext (authority never flipped, plaintext-readable) or resumable via
  `resume_pending_enable(_cold)` (which rebuilds decrypt-capable managers).

The one genuinely new runtime behavior is **restarting the background
persistence thread in-process** after the authority flip + ledger clear, which is
now safe because `install_index_keyring` has made `manager.keyring()` `Some` and
the `admin.rs:70` guard no longer fires.

---

## 7. Concrete step sequence

Lock annotations: which lock (if any) each step may hold. `LayerStatus`
transitions are the durable breadcrumbs. Steps unchanged from today are marked
*(existing)*; new steps are marked **(NEW)**.

0. **Preconditions (no side effects)** *(existing, `:227-268`)* — durable
   `persistence_manager` present (else `FailedPrecondition`, ephemeral); refuse if
   `encryption_manager.is_some() || wal.is_encrypted()` or the durable authority
   already `enabled`; compute `cold_in_scope = self.historical.read().has_tiered_storage()`
   (brief `historical` read, released immediately); resolve
   `algorithm = self.enable_algorithm()`; build the WAL keyring early
   (`build_enable_wal_keyring`) to fail fast on a bad key. *Holds: brief
   `historical.read()` only for the `has_tiered_storage()` probe.*

1. **Quiesce the background persist thread** *(existing, `:274-279`)* —
   `persistence_tracker.signal_shutdown()`; `persistence_thread_handle.take()` +
   `join()`. The worker's shutdown final-persist runs while still plaintext.
   *Holds: none (thread handoff).*

2. **Synchronous full persist while STILL PLAINTEXT** *(existing, `:291`)* —
   `self.persist_indexes()` captures all pre-enable current + historical state
   into the durable snapshot. Safe: WAL not yet encrypted, so the `admin.rs:70`
   guard does not fire. *Holds: internal persist locks (leaf).*

3. **Write enable ledger — breadcrumb #1** *(existing, `:299`)* —
   `write_enable_ledger(&manager, &key_source, /*index*/ true, cold_in_scope)`;
   stamps the KCV. Ledger: `wal=Pending`, `index=Pending`, `checkpoint=Pending`,
   `wal_retire=Pending`, `cold=Pending` iff `cold_in_scope`. *Holds: none.*

4. **Per-tier wrap + LIVE install, lock-order-safe:**

   a. **WAL** *(existing install, `:303-306`)* —
      `self.wal.install_wal_keyring(keyring).map_err(map_wal_install_err)?`
      (seal v13 → store keyring in presence cell → reopen v16, under the
      coordinator `writer` mutex) → `mark_wal_complete` → ledger `wal=Complete`.
      *Holds: `wal`-class locks internally (seal hand-off); the store closure
      touches only the presence cell — no lock ordered after `wal`.*

   b. **Index** — `wrap_enable_index_files(&manager, &key_source, algorithm)`
      (plaintext → AEIX on disk, idempotent, skips AEIX) *(existing, `:310`)* →
      **(NEW)** `manager.install_index_keyring(IndexKeyring::single(
      build_enable_index_cipher(&key_source, algorithm)?))
      .map_err(map_index_install_err)?` → `mark_index_complete` → ledger
      `index=Complete` AND `checkpoint=Complete`. After this, `manager.keyring()`
      is `Some` and the `admin.rs:70` window is closed. *Holds: index manager's
      leaf `install_lock` only.*

   c. **WAL retire** *(existing, `:321-322`)* —
      `retire_enable_plaintext_wal(&self.wal)` (retires sealed v13 segments; the
      encrypted snapshot from steps 2+4b holds every pre-enable record) →
      `mark_wal_retire_complete` → ledger `wal_retire=Complete`. *Holds:
      `wal`-class internally.*

   d. **Cold (iff `cold_in_scope`)** *(existing wrap, `:328-338`; NEW install)* —
      `let tiered = self.historical.read().tiered_storage_arc()?` (take
      `historical.read()` ONLY to clone the `Arc`, then the read guard is
      released before the wrap/install);
      `cold_cipher = build_enable_cold_cipher(&key_source, algorithm)?`;
      `tiered.cold_storage().wrap_plaintext_cold_values(&cold_cipher,
      ENABLE_KEY_VERSION)?` (bare → ACV1, cursor-resumable) → **(NEW)**
      `tiered.cold_storage().install_cold_keyring(ColdKeyring::single(cold_cipher))
      .map_err(map_cold_install_err)?` → `mark_cold_complete` → ledger
      `cold=Complete`. *Holds: cold store's leaf `install_lock`; NO `historical`
      guard held across the wrap/install (only the `Arc` clone was under it).*

5. **Flip authority BEFORE clearing ledger (Option A) — breadcrumb #5**
   *(existing, `:346-349`)* — `write_encryption_state_durable(manager.base_path(),
   &EncryptionState::enabled_with_algorithm(key_source.clone(), algorithm))`.
   Pins the resolved concrete algorithm. *Holds: none (atomic durable write).*

6. **Clear the ledger — breadcrumb #6** *(existing, `:352`)* —
   `clear_rotation_state(&manager)`. *Holds: none.*

7. **(NEW) Restart the background persistence thread in-process** — now that every
   live manager keyring is `Some`, respawn the worker (mirroring
   `config.rs:1418` `spawn_background_persistence_thread`, re-arming
   `persistence_tracker` and storing the new `persistence_thread_handle`). The
   handle is now fully operational encrypted; the reopen requirement is dropped.
   *Holds: none (thread spawn).* Also update: the loud reopen-contract doc block
   (`encryption_enable.rs:160-222`), the `EnableReport` docs, and the
   `admin.rs:60-79` guard commentary to state the window now closes at step 4b.

**Resume paths (unchanged shape, must stay correct):** `resume_pending_enable`
(`:391`) and `resume_pending_enable_cold` (`:503`) run at `open()`; they
KCV-verify first (`verify_resumed_source_kcv`, `:408`), then finish any
`Pending` layer idempotently. On resume the managers/stores are freshly built by
`enable_resume_ciphers` with `Some` keyrings, so **no live install is invoked on
the resume path** — the live installs matter only to the crash-free in-process
path. `fail_if_pending_enable_cold_without_tier` (`:557`) fails loud if a cold
layer is `Pending` but no cold tier exists on reopen.

---

## 8. Risks & edge cases as TEST CASES

Every risk maps to a named test. Red = expected-fail before the driver lands;
Green = expected-pass after. Interruption points reference C0–C4 (existing) and
the new P2/P4/P6/P7 from the brief.

| # | Risk | Test name | Asserts | Red→Green |
|---|---|---|---|---|
| T1 | Live installs never happen; reopen still mandatory | `enable_is_live_no_reopen_required` | After `enable_encryption` returns, the SAME handle serves reads/writes and `persist_indexes()` succeeds (guard not fired); `manager.keyring().is_some()` | Red (guard fires today) → Green |
| T2 | Plaintext index file survives the AEIX sweep | `no_plaintext_index_file_survives_aeix_sweep` | Whole-dir sweep: every persisted index file (manifest/interner/graph/temporal/temporal_adjacency/vector-meta + native usearch) satisfies `is_encrypted_index(bytes)` | Green (extends existing sweep) |
| T3 | Authority flips before all bytes wrapped (split-key) | `authority_flip_precedes_ledger_clear` | Drives the LIVE `enable_encryption` and injects a crash at the exact seam BETWEEN the durable authority flip and the ledger clear (thread-local `enable_test_hooks` seam, `src/db/encryption_enable.rs`); asserts on-disk (authority `enabled`, ledger still PRESENT, every layer `Complete`) then that resume is a clean no-op clearing the ledger and re-wrapping no tier | Green (implemented — closes GAP-1) |
| T4 | Seam install nests under an ordered lock → deadlock | ~~`enable_is_lock_order_safe`~~ **— covered by structural concurrency review, not a runtime test** (see note ‡ below) | `&mut self` serializes handle access; each seam install (`install_wal_keyring`/`install_enable_index_keyring`/`install_enable_cold_keyring`) takes only its own leaf lock; the cold path clones the tiered `Arc` under a temporary `historical.read()` and RELEASES the guard before wrap/install — no earlier ordered primitive is held across a later one. There is no nested lock acquisition to invert. | Structural (no meaningful runtime test) |
| T5 | Concurrent writes during migration corrupt/split state | ~~`concurrent_create_edge_during_migration`~~ **— not writeable through the public API** (see note ‡ below) | `enable_encryption(&mut self)` needs an exclusive borrow, so the borrow checker forbids a second handle method (e.g. `create_edge`) racing it on the same handle — a true concurrent-write race cannot be expressed. The cross-handle concurrent-orphan case is separately covered by Issue #3416's first-committer-wins `ValidationFailed` abort. | Structural (unwriteable as a true race) |
| T6 | **C0 / P0** crash before ledger | `resume_c0_crash_before_ledger_reopens_plaintext` | No ledger → plaintext reopen, authority `disabled`, all layers plaintext | Green (existing C0) |
| T7 | **C1 / P1** crash after ledger before WAL roll (wal+index Pending) | `resume_c1_crash_after_ledger_before_wal_roll` | Resume installs+rolls WAL, wraps index, flips only after; converge encrypted | Green (existing C1) |
| T8 | **P2 (NEW)** crash after WAL install + index on-disk wrap but BEFORE live `install_index_keyring` / `mark_index_complete` | `resume_after_index_wrap_before_install` | Reopen rebuilds index manager under enable index DEK (`enable_resume_ciphers`, keyring `Some`); `resume_pending_enable` re-runs the idempotent wrap; converge; whole-dir AEIX sweep passes | Red (no driver) → Green |
| T9 | **C3 / P3** crash after WAL roll before authority flip | `resume_c3_crash_after_wal_roll_before_flip` | Pre-replay `install_pending_enable_wal_keyring` decrypts replay; resume flips+clears | Green (existing C3) |
| T10 | **P4 (NEW, cold)** crash after cold on-disk `wrap_plaintext_cold_values` but before live `install_cold_keyring` / `mark_cold_complete` | `resume_after_cold_wrap_before_install` | `resume_pending_enable` defers (cold Pending); `resume_pending_enable_cold` rebuilds cold store under cold DEK and re-runs the cursor-resumable wrap; converge; `raw_node_value_is_acv1_for_test` true for all | Red → Green |
| T11 | **P4 limbo** crash mid-cold, reopen with NO cold tier | `resume_pending_enable_cold_without_tier_fails_loud` | `fail_if_pending_enable_cold_without_tier` returns a loud error; no silent limbo, authority not flipped | Green (existing) |
| T12 | **C4 / P5** crash in flip→clear gap | `resume_c4_crash_in_flip_clear_gap` | Authority `enabled`, ledger present → `resume_pending_enable(_cold)` clears; rotation resume paths skip the enable ledger | Green (existing C4) |
| T13 | **P6 (NEW, KCV)** source secret changed out-of-band between start and resume | `resume_kcv_mismatch_retains_ledger` | `verify_resumed_source_kcv` refuses BEFORE any wrap; error contains `"KCV"`+`"does not match"`; ledger RETAINED; authority NOT flipped | Green (existing KCV test; must still hold with live-install driver) |
| T14 | **P7 (NEW, in-process retry)** re-invoke live driver on a handle that already installed a seam | `double_install_is_idempotent_on_reinvoke` | `*KeyringAlreadyInstalled` from index/cold seam mapped via `map_*_install_err` to FailedPrecondition and treated as continue; no double-wrap; converge | Red → Green |
| T15 | Genuine seam I/O fault misclassified as idempotent | `map_index_install_err_distinguishes_double_install_from_io_fault` / `map_cold_install_err_...` | Only `Index/ColdKeyringAlreadyInstalled` → FailedPrecondition; a genuine fault stays INTERNAL (mirrors existing `map_wal_install_err` unit test at `:976`) | Red → Green |
| T16 | Worker respawn writes plaintext over AEIX | `restarted_worker_persists_encrypted_after_enable` | After the in-process restart, a mutation + worker persist produces AEIX files only (whole-dir sweep) | Red → Green |
| T17 | WAL not encrypted post-enable | `enable_encryption_encrypts_wal_flips_authority_clears_ledger` | `wal.is_encrypted()`, authority `enabled`, ledger cleared (existing happy-path, extended with the live-install assertions) | Green (existing) |
| T18 | Ephemeral / double-enable misuse | `enable_on_ephemeral_refused` / `double_enable_refused` | `FailedPrecondition` on ephemeral and on already-encrypted (existing) | Green |

Every convergence test additionally runs the invariant sweep from the brief §9:
whole-dir AEIX sweep + `raw_node_value_is_acv1_for_test` + `wal.is_encrypted()`
+ authority `enabled`, confirming NO layer is bare/plaintext under an `enabled`
authority and NO layer is encrypted under a `disabled`/absent authority.

**‡ T4 / T5 — structural-review coverage, deliberately NOT runtime tests.**
Both rows were originally sketched as runtime tests but, on implementation, a
faithful runtime test is either meaningless or unwriteable, so they are covered
by structural concurrency review instead (evidence at the file:line below):

- **T4 (`enable_is_lock_order_safe`).** The lock-order hazard a runtime/loom test
  guards against is a *nested* acquisition that could invert (the reason the
  existing `tests/havoc_loom_flush_coordinator.rs` model exists — it has a real
  `writer → sync_handle` nesting). The enable path has **no such nesting**:
  `enable_encryption_migrate` (`src/db/encryption_enable.rs:448`) installs each
  live keyring through a leaf-lock-only seam
  (`self.wal.install_wal_keyring` `:466`; `install_enable_index_keyring` `:479`;
  the cold install `:510`), and the cold path takes `historical.read()` ONLY to
  clone the tiered `Arc` and drops that guard *before* the wrap+install
  (`:501`–`:511`). No ordered write-path primitive (`current_timestamp` / `wal` /
  `historical` / `temporal_indexes` / adjacency, per CLAUDE.md's lock-acquisition
  order) is held across a later one, and the worker restart is a plain thread
  spawn holding no ordered lock (`restart_persistence_worker` `:541`). A loom
  model of this path would have to fabricate a nesting that does not exist; a
  liveness "drive enable, assert no hang" smoke test proves only liveness, not
  ordering. So the invariant is established by the structural trace, not a runtime
  assertion.
- **T5 (`concurrent_create_edge_during_migration`).** `enable_encryption(&mut self)`
  requires an exclusive borrow of the handle, so the borrow checker forbids a
  second handle method (a `create_edge`) from running concurrently on the same
  handle — a true concurrent-write-vs-migration race **cannot be expressed**
  through the public API, so there is nothing for a runtime test to exercise. The
  genuinely-concurrent hazard in the neighbourhood — a cross-handle
  `create_edge`/`delete_node` racing to orphan an edge — is already covered by
  Issue #3416's first-committer-wins `ValidationFailed` abort under the
  commit-serialization (`historical` write) guard, independent of this driver.

This is **structural-review coverage, not runtime coverage**; it is recorded here
and in the AC-evidence dossier (§D GAP-2) so the traceability matrix stays honest
rather than naming two tests that were never (and should never be) written.

---

## 9. Out of scope / follow-ups

- **Streaming / incremental wrap** under the live keyring (B8) — v1 keeps the
  crash-proven batch/cursor wrap passes.
- **Per-tier live-install `LayerStatus`** in the ledger (B14) — unnecessary; the
  seam presence cell reconstructs process-local install state.
- **CLI live-enable** — the CLI (`aletheia encryption enable`,
  `src/bin/aletheia.rs`) intentionally exits so the next invocation reopens; it
  is a fresh process and always goes through `enable_resume_ciphers`. Converting
  it to a single-process live enable is a follow-up.
- **Disable-side live path** (`disable_encryption`) — a symmetric follow-up; this
  driver is enable-only.
- **Continuing WRITES through the returned handle during the quiesced window** —
  fully supported once the worker restarts (step 7); no separate v1 work.

### Known-OK test failures to disclose (sandbox, NOT regressions)

- **2 uid-0 havoc fails** — the sandbox runs as uid 0, so chmod-based fault
  injection is a no-op for root: `test_flush_deadlock_on_io_error`
  (`tests/havoc/havoc_flush_deadlock.rs`) and `test_metadata_corruption_on_error`
  (`tests/regression_flush_corruption.rs`).
- **ENOSPC on `--all-features`** — the instrumented / `--all-features` build dies
  at link (ld Bus error / ENOSPC). Verification baseline instead:
  `cargo test --features mcp-server --no-fail-fast`; free space with
  `cargo clean -p aletheiadb`.
