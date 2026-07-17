# GDPR Crypto-Shred — Design Doc (Issue #3359)

**Lane:** Wave-8 Lane G · **Status:** DESIGN (pre-implementation, coordinator collision-check pending) · **Date:** 2026-07-16

## 1. Problem & shred unit

GDPR "right to erasure" over an append-only bi-temporal store: you cannot physically delete history without breaking temporal invariants and the provenance hash chain. **Crypto-shred** solves this by encrypting the erasable payload under a **per-subject key** and erasing by **destroying that key** — the ciphertext may remain across every tier but becomes permanently undecryptable.

**Shred unit (from AC1, verbatim):** a caller-designated **erasure subject** — "grouping one or more entities and/or specific property keys under a subject identifier." It is:
- **Caller-driven / explicit** (no auto-PII discovery — out of scope).
- Spans **whole entities AND/OR specific property keys**.
- Designatable **at creation or later**.
- NOT per-node and NOT per-namespace. (Confirmed: no namespace primitive exists in the repo; the "namespaces lane" the coordinator flagged does not exist. The subject axis is designed standalone.)

**Explicitly out of scope (v1):** full at-rest encryption (#477–491 — align key-provider config, don't duplicate), auto-PII discovery, topology/structure erasure, retention scheduling, cross-shard erasure.

## 2. Six-hats analysis

- **White (facts):** Encryption stack today = `KeyProvider → MEK → HKDF-SHA256 → 4 component DEKs (wal/index/cold/checkpoint) → AEAD cipher`. No per-record/per-subject key axis exists. Three key-versioned at-rest wrappers (index AEIX, WAL KEYVERSIONED, cold ACV1) all self-describe key-version and fail loudly on wrong key. `write_durable` gives a 4-step-fsync breadcrumb (temp→sync_all→rename→parent-dir fsync). `encryption.state` is a fail-closed durable authority file. `version_leaf` in the provenance chain currently hashes **plaintext** properties. Embeddings are first-class `PropertyValue::Vector` and HNSW needs plaintext floats.
- **Red (feelings/risk intuition):** The scariest failure is a **silent** one — claiming a subject is erased when a plaintext copy survives in some tier (usearch `.bin`, a stale backup, the WAL). Second scariest: breaking `verify` for every user the moment anyone erases anything.
- **Black (caution):** Deriving subject keys from the MEK (`HKDF(MEK, subject_id)`) is NOT crypto-shred — the key stays re-derivable while the MEK lives; that's access-revocation, and it fails AC1. Overwriting a registry file does not guarantee media-level erasure (SSD wear-leveling). Late designation cannot retroactively shred data already written in plaintext without rewriting history.
- **Yellow (optimism):** The at-rest wrappers (esp. ACV1) are exactly the "self-describing, loud-fail" model a sealed envelope needs. `write_durable` + the rotation ledger give a ready-made crash-safe breadcrumb. `is_tombstone` already rides the chain leaf, so an erasure tombstone reuses existing machinery. Hashing the **stored ciphertext** (which survives erasure) instead of plaintext makes the chain erasure-stable for free.
- **Green (creative):** Exclude designated embeddings from the shared plaintext HNSW entirely (they become non-searchable but shreddable) — trades semantic search for erasability, the correct GDPR bias. Per-subject index partitions are a future path to restore search.
- **Blue (process):** Ship serialized draft PRs, foundation first, no rotation.rs touch in slice 1 (respect the encryption-migration session's ownership); on-disk formats stay DRAFT until user sign-off.

## 3. Brainstorm & reverse-brainstorm

**Brainstorm (how to make payload unrecoverable):** random per-subject DEK; MEK-wrapped DEK registry; physically-separate key blob per subject; sealed-envelope PropertyValue; exclude designated vectors from HNSW; hash-of-ciphertext chain leaf; erasure tombstone tx; signed attestation; sentinel byte-scan proof; fail-closed erased registry; ordered-fsync breadcrumb; zeroize on erase.

**Reverse-brainstorm (how would we ACCIDENTALLY leave data recoverable?):**
1. Derive the subject key from the MEK → still re-derivable after "erasure." → **Mitigation:** random independent DEK, not derived.
2. Index a designated embedding into the shared plaintext HNSW `.bin`. → **Mitigation:** exclude designated vectors from the plaintext index.
3. Let a designated property reach a tier before sealing. → **Mitigation:** seal at the single write choke point, before any tier sees it.
4. Keep the plaintext in the chain leaf so verify recomputes plaintext. → **Mitigation:** bind ciphertext (erasure-stable) into the leaf.
5. Leave the wrapped DEK in a stale registry copy or a WAL log of the registry write. → **Mitigation:** atomic temp→rename single-live-file registry; registry mutations are not WAL-logged payloads.
6. Attempt to re-derive from a checkpoint of in-memory key cache. → **Mitigation:** subject DEKs are Zeroizing, never checkpointed in plaintext.
7. Report success mid-erasure after a crash leaves the key half-present. → **Mitigation:** breadcrumb + fail-closed resume; success only after key gone AND tombstone durable.

## 4. Key hierarchy (selected)

```
KeyProvider ──► MEK ──HKDF──► subject-wrapping DEK (info "aletheiadb-subject-wrap-dek-v{kv}")
                                     │ wraps (AEAD)
                                     ▼
                         random per-subject DEK  (32 bytes, CSPRNG, Zeroizing)
                                     │ encrypts (AEAD)
                                     ▼
                    designated erasable payload (property values / vector floats)
```

- **Per-subject DEK is random**, generated at designation — NOT derived from the MEK. This is the crux: destroying it is irreversible even with the MEK.
- The DEK is stored **wrapped under the subject-wrapping DEK** in a durable **subject keyring authority file** (`{data_dir}/subject_keyring.dat`, modeled on `encryption.state` + `write_durable`; Zeroizing in memory, redacting Debug, secrets never logged).
- A **designation registry** records `subject_id → {entity ids, property keys}` and lifecycle state (`Active | Erased`), same durability pattern.
- **Erasure = physically remove the wrapped DEK blob from the keyring file** (rewrite atomically without it) + zeroize the in-memory copy. The MEK cannot regenerate a random key ⇒ all ciphertext becomes permanently undecryptable.

**Why not `HKDF(MEK, subject_id)`?** Rejected: re-derivable while the MEK lives → access-revocation, not erasure; fails AC1.

## 5. Sealed payload & what "shredded" means per tier

At the write choke point, a designated entity/property's erasable value is replaced with a **self-describing sealed envelope** (magic `SUBJ`, subject id, key-version, `AEAD(nonce||ct||tag)`), modeled on ACV1's loud-fail contract. Every tier then carries only ciphertext:

| Tier | What holds the sealed envelope | Post-erasure state |
|---|---|---|
| Hot RAM (index-persisted) | envelope bytes in the property store; index-persistence writes ciphertext | undecryptable |
| WAL segments | logged envelope bytes | undecryptable |
| Warm/Cold (redb ACV1) | ACV1 wraps the envelope (double-encrypted) | undecryptable |
| Checkpoints | envelope bytes | undecryptable |
| `.albk` backup (bump to v6) | envelope bytes passed through | undecryptable (if backup made after designation) |
| HNSW vector index | **designated vectors are NOT inserted** into the shared plaintext `.bin` | no plaintext floats exist to shred |
| Labels / ids / temporal coords / topology | plaintext (AC3 — documented non-erasable) | preserved |

**Sharpest edge — embeddings:** usearch requires plaintext floats; the shared `.bin` cannot be selectively shredded. **v1 decision:** designated embeddings are sealed and **excluded from the shared HNSW** ⇒ not returned by `find_similar`/ANN. Documented limitation; per-subject index partitions restore search later.

## 6. Provenance-chain compatibility (AC4 — the hardest constraint)

`version_leaf = SHA256(LEAF_DOMAIN || canon(version))` binds a version's
`properties` (`src/provenance_chain/canonical.rs`). If it bound the **plaintext**
of a sealed property, destroying that plaintext would break `verify` for everyone.

**Status — already holds by construction as of #3689 (seal-before-apply):** the
erasure-stable ciphertext leaf is **not** a new `version_leaf` rewrite in this
slice. As of the PR-1b seal-at-write path (#3689), a designated property is
sealed to a `PropertyValue::Bytes(SUBJ…)` envelope in the write buffer *before*
`apply_changes`, so both the current and historical tiers store **ciphertext**.
The chain leaf is computed *post-commit* by the sealer (`Sealer::seal_one`,
`src/provenance_chain/engine.rs`) from `DbVersionSource`, which reconstructs
properties via the **raw** `reconstruct_*_properties` storage path
(`src/db/chain_source.rs`) — crypto-shred-**unaware**, so it returns the stored
sealed bytes, never plaintext. `verify` recomputes from the *same* raw path, and
`erase_subject` destroys only the subject key (never the stored envelope). So,
with **zero special-casing** in `Canon::value`/`version_canonical`, the leaf
already binds the stored sealed-envelope bytes for a designated property:
- `verify` recomputes hash-of-envelope-bytes → matches post-shred (chain stays valid). ✓ AC4
- Mutating the envelope ciphertext changes the hash → tamper still caught. ✓ AC4 tamper test
- Non-designated properties are unchanged: with no designation, historical holds
  plaintext and the leaf binds it exactly as before.

**Consequence — Slice 2 is regression proof + a guardrail, not a leaf rewrite:**
no `canonical.rs`/`verify.rs` code change and **no on-disk format change** — the
`hash(plaintext)` → `hash(ciphertext)` shift for sealed properties landed with
#3689, not here. Slice 2 delivers (1) regression tests
(`crypto_shred_ac4_tests` in `src/db/chain.rs`) proving `verify_chain` still
passes after a crypto-shred (node and edge) and that a byte-flip of the stored
envelope is still detected, plus a non-designated no-regression check; and (2) a
guardrail comment on `DbVersionSource::fetch_locked` pinning it to the raw
`reconstruct_*_properties` path — it MUST NOT be rerouted through the db-API
unsealing boundary (`materialize_shred`/`unseal_*_view`), which would make the
leaf bind plaintext and break AC4.

No separate commitment store needed — the ciphertext IS the erasure-stable commitment.

## 7. Erasure operation — crash-consistency (breadcrumb pattern)

Reuses `write_durable` (temp→sync_all→rename→parent-dir fsync) and a rotation-ledger-style resumable marker:
1. Durable breadcrumb: "erasing subject S — started."
2. Rewrite subject keyring atomically **without** S's wrapped DEK (ordered fsync).
3. Zeroize in-memory subject DEK for S.
4. Record **erasure tombstone as a normal transaction** (WAL): subject id, entity/version counts, timestamp.
5. Produce **signed attestation** (reuse `audit/signing.rs`): subject id, counts, timestamp — no content disclosed.
6. Mark breadcrumb complete.

**Crash resume (fail-closed):** breadcrumb present + key still in keyring → resume removal; key gone + tombstone missing → re-record tombstone; any ambiguity → treat S as erased (never re-expose). Idempotent; success reported only after key destroyed AND tombstone durable.

## 8. Read path (AC3)

Pre-erasure `AS OF` still returns structure (ids, temporal coords, label, topology). Sealed values whose key is destroyed return an **explicit erased indicator** — reusing the #3220 `{elided:true}` descriptor convention as `{erased:true, subject_id}` — never fabricated, never silently absent. Rust API surfaces a typed marker; MCP surfaces the JSON descriptor.

## 9. Verification — proving unrecoverability (AC5/AC6)

- **Sentinel byte-scan:** write a known sentinel into a designated property, run the full lifecycle (hot→cold→checkpoint→backup→WAL rotation), erase, then byte-scan EVERY artifact (WAL segments, index files, cold redb, `.albk`, checkpoint, usearch `.bin`) → **zero hits**; restart → read returns erased indicator, decrypt impossible.
- **Blast-radius fixture:** erasing subject A leaves subject B's reads byte-identical (read-equivalence). Erasing an undesignated entity → structured `FAILED_PRECONDITION`.
- **verify_decryptable analog:** assert the sealed envelope no longer decrypts (key gone).

## 10. Honest limits (documented, per coordinator mandate)

1. **Pre-designation / pre-key data cannot be retroactively shredded** without a history-rewriting migration. v1: late designation seals **forward only**; versions written before designation remain in plaintext and are out of boundary. (Prefer designation at creation.)
2. **Backups/audit-exports made before designation or before erasure are out of boundary** (AC7) — honest docs + operational procedure required.
3. **Topology/structure is not erased** — which entities/edges exist, labels, temporal coordinates persist (AC3, explicit v1 scope).
4. **Designated embeddings are not semantically searchable** in v1 (excluded from shared HNSW).
5. **Registry media-erasure is the operator's responsibility** — crypto-shred reduces the problem to destroying one small secret, but the file rewrite relies on the filesystem; document storage requirements (no wear-leveled media without secure-erase, etc.).
6. **Threat model:** MEK compromise *before* erasure combined with a captured registry copy exposes the subject; *after* erasure the wrapped blob is gone. Standard crypto-shred model.

## 11. Approaches considered

**Approach A — Random per-subject DEK, MEK-wrapped, in a durable keyring; erase = destroy the wrapped blob. (SELECTED)**
- Pros: true crypto-shred (irreversible even with MEK); reuses `KeyProvider`/`Cipher`/`write_durable`/ACV1 patterns; ciphertext-in-leaf keeps the chain valid; per-tier coverage falls out of the sealed envelope.
- Cons: adds a new durable authority file + registry; MEK rotation must re-wrap subject DEKs (deferred, coordination-gated); sealed-envelope codec touches the property write/read path.

**Approach B — Derived subject key `HKDF(MEK, subject_id)` + fail-closed "erased" registry. (REJECTED)**
- Pros: no key storage; simplest.
- Cons: **not crypto-shred** — key re-derivable from MEK; only enforces access control at the read gate; fails AC1's "unrecoverable" and AC5's byte-scan intent (a determined holder of MEK+ciphertext recovers plaintext). Rejected.

**Approach C — Per-subject independent keystore in an external KMS (one KMS key per subject); erase = KMS DeleteKey. (DEFERRED)**
- Pros: strongest media-erasure guarantee (key never on local disk).
- Cons: hard KMS dependency, per-subject KMS object explosion, latency on every designated read; over-scoped for v1 (issue says align with #477–491 provider config, not require KMS). The Approach-A keyring can be wrapped by a KMS-derived key later, so A is forward-compatible with C.

**Decision: Approach A**, with the subject-wrapping key forward-compatible with a KMS root (Approach C) as a later hardening.

## 12. Risks & edge cases → test cases

| # | Risk / edge | Test |
|---|---|---|
| R1 | Erased subject still decryptable via MEK | After erase, attempt decrypt with live MEK → fails; key absent from keyring |
| R2 | Plaintext survives in a tier | Sentinel scan across WAL/index/cold/checkpoint/`.albk`/usearch → 0 hits |
| R3 | Chain verify breaks post-shred | `verify_chain` passes after erasing a subject with sealed props |
| R4 | Tamper no longer caught | Mutate a sealed envelope byte → `verify_chain` fails |
| R5 | Blast radius | Erase A → B reads byte-identical (read-equivalence fixture) |
| R6 | Erase undesignated entity | Returns `FAILED_PRECONDITION`, no writes |
| R7 | Crash mid-erase (key gone, tombstone missing) | Restart resumes → tombstone recorded, subject stays erased |
| R8 | Crash mid-erase (breadcrumb set, key present) | Restart resumes removal → key gone |
| R9 | Designated embedding leaks into HNSW | Designated vector absent from `find_similar` results; `.bin` sentinel-clean |
| R10 | Read after erase | `AS OF` returns structure + `{erased:true}` indicator, never fabricated value |
| R11 | Re-erase idempotency | Second `erase_subject` is a no-op returning the prior attestation |
| R12 | Non-designated perf regression | Bench: non-designated write/read throughput unchanged (0 regression) |
| R13 | Designated perf envelope | Bench: designated single-hop read < 2µs, ≥ 90% write throughput at 10% designated |
| R14 | Late designation | **PR-1a: static designation + reserved-key exemption rule only** (the designation set and `should_seal_key`/`any_should_seal` are unit-tested); the forward-seal write-path behavior (prior version still plaintext, new version sealed) is **deferred to PR-1b**, since PR-1a has no live seal/unseal write path |
| R15 | Secret leak | Debug/Display of keys redacted; grep logs/errors for key bytes → none |

## 13. Implementation slices (serialized draft PRs — base=trunk, never stacked, no force-push)

- **Slice 1 — Foundation (NO rotation.rs / wal_encryption.rs / reencrypt.rs touch):** subject key axis (random DEK, MEK-wrap, durable `subject_keyring.dat` + designation registry via `write_durable`); Rust API `designate_subject` / `erase_subject` (breadcrumb → key destroy → tombstone tx → signed attestation); sealed-envelope write/read choke point; erased read indicator; exclude designated vectors from HNSW. Tests R1–R11, R14, R15. **On-disk formats (keyring, registry, sealed envelope) → DRAFT, coordinator surfaces for user sign-off.**
- **Slice 2 — Chain compat (AC4):** the erasure-stable (ciphertext) commitment in the version leaf **already holds by construction as of #3689** (seal-before-apply + post-seal leaf capture from the raw historical reconstruct — see §6). This slice is therefore **regression proof + a guardrail, NOT a `version_leaf` rewrite and NO on-disk format change**: regression tests (`crypto_shred_ac4_tests`, `src/db/chain.rs`) proving `verify_chain` still passes after a crypto-shred (node + edge) and that an envelope-byte tamper is still caught, plus a guardrail comment pinning `DbVersionSource::fetch_locked` (`src/db/chain_source.rs`) to the raw `reconstruct_*_properties` path (never the unsealing boundary). Tests R3, R4.
- **Slice 3 — Full-tier completeness (MAY touch WAL/cold — coordinate with encryption-migration session):** cold/checkpoint/`.albk` v6 pass-through + full sentinel scan across ALL artifacts. Tests R2, R9.
- **Slice 4 — Surfaces:** CLI `aletheia erase-subject`, MCP tool (admin-gated #3350), attestation format, user guide + honest-limits doc.
- **Slice 5 — Coordination-gated:** MEK-rotation re-wrap of the subject keyring (touches `rotation.rs`) — done with / handed to the encryption-migration session.
- **Perf (woven through 1 & 3):** benches for R12/R13.
  - **PR-1a note:** R12/R13 (perf benches) and AC8 are **deferred to PR-1b** —
    PR-1a has no live seal/unseal data path to bench yet, so there is nothing
    meaningful to measure at the foundation level.

**Coordination boundary:** the encryption-migration successor session owns `src/encryption` + `src/storage/wal` keyring code and #3616 PRs 2–4; slice 1 deliberately avoids those files; slices 3 and 5 require coordinator-brokered coordination before touching `wal_encryption.rs` / `rotation.rs` / `reencrypt.rs`.

## PR-1b delivered / deferred (property-path integration)

**Delivered in PR-1b** (wires the PR-1a cryptographic core into the live
data path — no `src/encryption/`, `rotation.rs`, `wal_encryption.rs`, or
`reencrypt.rs` edits):

- **Reverse designation index** on `SubjectKeyring`
  (`HashMap<(is_node,u64), Vec<subject_id>>`): maintained in `upsert` (so it
  grows with `designate`'s merge/new arms), rebuilt in `load_from_sidecar`, left
  intact by `erase_in_memory`/`force_erased` (so reads still report erased).
  Plus lock-free gates `has_active` / `has_any` (AtomicBool) so a database with
  no active designations pays zero per-write cost and one with no designations
  at all pays zero per-read cost.
- **Seal at write:** `apply_node_write` / `apply_edge_write` rebind `properties`
  through a single `seal_map` at the top, so BOTH the current and historical
  tiers receive **byte-identical** sealed-envelope ciphertext (one seal, random
  nonce shared). `WriteTransaction` carries a prepared `SealingContext`
  (`Arc<CryptoShredState>` + memoized wrap cipher + algorithm), populated in
  `write_transaction()` only when active designations exist (gated
  `#[cfg(feature="audit-export")]`). Reserved keys (`__aletheia_*` / `__shred_*`,
  incl. the `__aletheia_ns` namespace marker) are never sealed — free via
  `should_seal_key`. Write-site context binds `entity_id ‖ property_key`. A
  sealed `Vector` becomes `Bytes`, so `try_index_vector`'s `as_vector()` gate
  naturally **excludes designated embeddings from the shared HNSW** (no
  `try_index_vector` change).
- **Unseal at read:** `AletheiaDB::materialize_shred` + `unseal_node_view` /
  `unseal_edge_view`, hooked at `get_node` / `get_edge` (current fast path) and
  `get_node_at_time` / `get_edge_at_time` (point-in-time). Active subjects →
  plaintext in place; erased subjects → opaque ciphertext left untouched +
  `ShredStatus::Erased{subject_id}` recorded in a side-channel `ShredStatusMap`
  (NOT a `PropertyValue` variant, NOT a wire change). The **restore path**
  (`restore_property_map`) stays a pure ciphertext passthrough — never unsealed
  there (no keyring / interner-guard hazard).
- **MCP erased marker:** `property_value_to_json` renders a surviving `SUBJ`
  envelope for a known subject as `{"type":"sealed","erased":<bool>,"subject_id":<id>}`
  (mirrors the #3220 vector-elision descriptor); a `Bytes` value whose subject
  the keyring does not recognize renders as ordinary bytes.
- **Erase op unchanged from PR-1a** (key destruction + durable keyring
  Erased-state + signed attestation). **No `db.write` tombstone added here.**

**Deferred (explicitly out of PR-1b):**

- **WAL-transaction erasure tombstone + changefeed observability** → a
  **#3413-aligned follow-up**. It needs a WAL-format extension to survive crash
  recovery, and emitting it from `erase()` would take `historical`/`wal` while
  the keyring `Mutex` is held, inverting the lock order (seams §6). The durable
  keyring Erased record IS the erasure record for now.
- **Cold-tier + `.albk` sentinel completeness** → **slice 3** (the PR-1b
  sentinel byte-scan covers only the in-scope hot tiers: WAL segments,
  index-persistence files incl. usearch `.bin`, current-tier persisted files,
  checkpoint if present — NOT cold redb or `.albk`).
- **Chain-leaf ciphertext binding (AC4)** → **slice 2**.
- **Exhaustive every-query-path unseal** → the Rust unseal hook covers the
  primary single-entity boundaries (`get_node`, `get_edge`,
  `get_node_at_time`, `get_edge_at_time`). Paths that still return **raw sealed
  `Bytes`** (opaque ciphertext — safe, never plaintext, just not yet
  unsealed/marked at the Rust level): `with_node` (zero-copy borrow),
  `list_nodes` / `list_edges`, adjacency (`get_outgoing_edges` /
  `get_incoming_edges`), `traverse`, `find_nodes_at_time`, the query executor
  (`iterators.rs`) and `read_tx` reconstructs. For all of these the MCP funnel
  still renders the erased descriptor for any envelope that reaches it, so an
  erased value is never surfaced as plaintext on the MCP surface. Extending the
  Rust unseal hook to these is a follow-up.

## AC4 disclosure — sealed-property verify semantics

The provenance hash chain's per-version leaf (`version_leaf`,
`src/provenance_chain/canonical.rs`) binds a version's `properties`. Under
crypto-shred, a designated property is stored as a **sealed envelope**
(ciphertext), and the leaf therefore binds the **stored ciphertext** for that
property — not its plaintext. The consequence, disclosed here rather than
hidden:

- For a sealed property, `verify` attests **ciphertext integrity**, not
  plaintext integrity. The chain proves "this exact sealed envelope was
  recorded at this version and has not been altered."
- A sealed property whose **key has been destroyed** still verifies: the
  ciphertext bytes survive erasure (only the key is gone), so the leaf digest
  is unchanged and the chain stays valid post-shred (AC4).
- Any mutation of the stored ciphertext still changes the leaf digest, so
  tampering with a sealed value is still caught by `verify` (AC4 tamper test).
- Non-designated properties are unaffected: their plaintext is bound exactly as
  today.

This ciphertext-binding is the erasure-stable commitment — no separate
commitment store is needed, because the ciphertext IS the commitment that
survives key destruction. This binding **already holds by construction** as of
the seal-before-apply write path (#3689): sealing runs pre-apply so historical
storage holds the ciphertext envelope, and the chain leaf is computed post-commit
from that raw stored envelope (`DbVersionSource` → `reconstruct_*_properties`),
with no special-casing in `canonical.rs`/`verify.rs`. **Slice 2** therefore does
**not** rewrite `version_leaf` and makes **no on-disk format change** — it adds
regression tests + a guardrail comment locking `DbVersionSource` to the raw
reconstruct path (see §6). Existing non-crypto-shred chains are unaffected.

## Reconciliation with the prior VANTAGE hard-delete spec

An earlier spec — `docs/specs/VANTAGE_SPEC_GDPR_HARD_DELETE.md`, on branch
`origin/docs/vantage-gdpr-spec-*` — proposes **physical eviction**: a
`tx.evict_node(id)` op that scrubs a node's data from WAL, hot memory,
checkpoints, and the cold tier, using a durable **evict-filter blocklist** to
hide data immediately and a **background vacuum/compaction** job to physically
rewrite immutable tiers later, so an `AS OF SYSTEM_TIME` query returns nothing
"as if the node never existed."

That approach **conflicts** with the append-only + bi-temporal + hash-chain
invariants that #3359 AC3/AC4 explicitly **preserve**:

- "As if it never existed" **destroys** version structure, temporal
  coordinates, labels, and topology — exactly what AC3 keeps — and **removes
  chain leaves**, breaking the #3351 provenance chain that AC4 requires to keep
  verifying.
- It does **not** address the plaintext **HNSW vector index** (raw floats in the
  usearch `.bin`), nor **`.albk` backups** — two tiers it never mentions.
- Its immutable-tier story is *eventual* physical deletion via compaction, which
  is precisely what append-only + bi-temporal integrity makes expensive/slow,
  and which cannot reach already-exported `.albk` artifacts at all.

**Crypto-shred** (cryptographic forgetting: content becomes permanently
unreadable everywhere at once via one destroyed key, while structure + chain
remain verifiable) **supersedes VANTAGE for erasure semantics**. It uniquely
covers the HNSW index (designated vectors are excluded from the shared
plaintext graph) and post-designation backups (they carry only ciphertext), and
it threads the append-only needle #3359 requires.

VANTAGE's evict-filter + background vacuum remains a possible **future
storage-RECLAMATION complement** — a way to reclaim disk from already-shredded
ciphertext (reclamation, not an erasure guarantee). The old VANTAGE spec branch
can therefore be **retired for erasure semantics knowingly**: it has no
implementation on its branch, and its two biggest gaps (HNSW plaintext,
chain/temporal preservation) are exactly what crypto-shred solves.
