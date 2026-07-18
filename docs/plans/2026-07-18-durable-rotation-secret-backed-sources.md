# Design: Durable key rotation to passphrase/KMS/Vault key sources (Issue #3620)

**Status:** DRAFT — orientation / approach proposal. No code written. Awaiting
approach sign-off before any implementation.

**Origin:** Issue #3620 (wave-6 triage residue), origin #3602. Related engines:
#3617 (MEK rotation), #3616/#3700 (enable), #3616/#3718 (disable), #3587/#3602
(KeyProvider hardening + the fail-closed refusal this design lifts).

---

## 1. What #3620 asks for (verbatim)

Issue #3620 has a title and a two-sentence body — there is **no formal
acceptance-criteria list** in the issue. Quoted verbatim:

> **Title:** Durable key rotation to passphrase/KMS/Vault key sources
>
> **Body:** The crash-breadcrumb rotation path cannot round-trip
> remote/derived secrets (passphrase KDF, KMS, Vault), so rotation to those
> sources fails closed today. Implement durable rotation that survives crash
> recovery for remote/derived key sources. Origin: #3602.
>
> Filed as wave-6 triage residue.

The origin PR #3602 (finding **G**) is where the fail-closed refusal was
deliberately introduced, and it names #3620 as the follow-up:

> **G [FEATURES] — rotation refusal ordering.** `refuse_unsupported_new_source`
> runs before any `load_mek`/network call. Test:
> `rotate_to_remote_source_refuses_without_reaching_endpoint` (Vault/KMS at an
> unreachable endpoint → clean refusal, no breadcrumb).

**Implicit AC (derived from the body + the code the body points at), for
sign-off:**

- **AC1.** Starting a rotation whose *new* key source is `PassphraseFile`,
  `Kms`, or `Vault` must succeed (no longer return the "not yet supported"
  `FailedPrecondition`/`PersistenceError`).
- **AC2.** A crash at any point during such a rotation must resume to a
  consistent state on the next `open()` — every layer either fully old-key or
  fully new-key, never a half-rotated DB that mis-reads.
- **AC3.** Resume must reconstruct the new-key cipher **without any plaintext
  secret having been written to disk** (the whole reason for the original
  refusal — see §3).
- **AC4.** The failure modes must stay *loud and resumable*: if the secret is
  unavailable at resume (KMS/Vault unreachable, passphrase/token env not
  supplied, wrong passphrase), startup must fail with an actionable error and
  leave the ledger intact so a later `open()` can retry — never silently drop
  the rotation or corrupt data.

> ⚠️ **Scope flag for coordinator:** #3620's title says "rotation", but the
> identical two-field-ledger limitation blocks the **enable** (#3700) and
> **disable** (#3718) engines too (§3.3). All three share the same ledger
> writer/reader. A fix that only unblocks `run_rotation` leaves enable/disable
> still refusing secret-backed sources. **Recommend treating the ledger-format
> fix as the shared primitive and lifting all three refusals together** (they
> are one code change plus three guard removals), but this is a scope decision
> for you. The rest of this doc is written for that shared-primitive framing.

---

## 2. Orientation: how key sources and the resume path work today

### 2.1 `KeyProviderConfig` — the five variants

`src/encryption/config.rs:97-162`. All variants and how each obtains the MEK:

| Variant | MEK obtained by | "Secret-backed"? | Plaintext secret in the config struct? |
|---|---|---|---|
| `File { path }` | read key bytes from a file (`FileKeyProvider`) | No | No (a path) |
| `Env { variable }` | read hex from an env var (`EnvKeyProvider`) | No | No (a var **name**) |
| `PassphraseFile { path, passphrase_env }` | KDF-unwrap the `AEKF` file using a passphrase read from `$passphrase_env` at build time | **Yes** | **No** — the passphrase is a var *name*; the file is a *wrapped* MEK |
| `Kms { key_id, encrypted_data_key, region, endpoint_url }` | AWS KMS `Decrypt` of the wrapped data key | **Yes** | **No** — `encrypted_data_key` is KMS-wrapped ciphertext (a wrapped MEK) |
| `Vault { address, token_env, mount, path, key_field, namespace, ca_cert }` | HTTP GET the KV-v2 secret using a token read from `$token_env` | **Yes** | **No** — the token is a var *name* |

**Critical observation:** *no* `KeyProviderConfig` variant contains a plaintext
secret. Passphrase/Vault hold env-var **names**; KMS holds a **wrapped**
(non-usable-without-the-CMK) blob. `build_provider()`
(`config.rs:248-342`) reads the *actual* secret (passphrase, Vault token) from
the environment **at call time** and moves it straight into the provider inside
a `Zeroizing` buffer; the secret never lands in the struct, a log, or an error.

The provider dispatch is centralized: `KeyProviderConfig::build_provider()` is
the single place every backend is constructed, and `get_mek()` returns the
32-byte MEK. `Kms`/`Vault` are feature-gated (`encryption-aws-kms` /
`encryption-vault`) and return `KeyProviderError::Unavailable` when not compiled
in.

### 2.2 Where and why rotation/enable/disable refuse secret-backed sources

There are **four** refusal sites, all with the same rationale ("the ledger can
only persist a File/Env reference without leaking a secret"):

1. **`refuse_unsupported_new_source`** — `src/db/rotation.rs:146-159`. Called
   first in `run_rotation` (`rotation.rs:761`). Fails on the *variant alone*,
   before any `load_mek`/network call:
   ```rust
   KeyProviderConfig::PassphraseFile { .. }
   | KeyProviderConfig::Kms { .. }
   | KeyProviderConfig::Vault { .. } => {
       let (provider_type, _) = new_source.describe();
       Err(StorageError::PersistenceError(format!(
           "index key rotation to a {provider_type} key source is not yet supported"
       )).into())
   }
   ```

2. **`write_ledger`** (defense-in-depth) — `src/db/rotation.rs:1269-1282`. The
   ledger *writer* itself refuses anything but File/Env:
   ```rust
   KeyProviderConfig::PassphraseFile { .. }
   | KeyProviderConfig::Kms { .. }
   | KeyProviderConfig::Vault { .. } => {
       let (provider_type, _) = ledger.new_source.describe();
       return Err(StorageError::PersistenceError(format!(
           "key rotation to a {provider_type} key source is not yet supported"
       )).into());
   }
   ```

3. **Enable engine** — `src/db/encryption_enable.rs:250-260`:
   ```rust
   match &key_source {
       KeyProviderConfig::File { .. } | KeyProviderConfig::Env { .. } => {}
       other => {
           let (provider_type, _) = other.describe();
           return Err(Error::FailedPrecondition(format!(
               "enabling encryption with a {provider_type} key source is not supported \
                (only file/env references can be persisted without leaking a secret)"
           )));
       }
   }
   ```

4. **Disable engine** — `src/db/encryption_disable.rs:205-217` (same shape,
   for the *current* source it must record to rebuild the decrypt cipher).

### 2.3 The exact resume-safety gap — it is *only* the ledger serialization

The durable breadcrumb is `{data_dir}/rotation.state`, a line-based file. The
`RotationLedger` struct (`rotation.rs:1075-1095`) carries
`new_source: KeyProviderConfig`, but the **on-disk encoding collapses it to two
strings** (`write_ledger`, `rotation.rs:1270-1291`):

```
new_source_kind={file|env}
new_source_value={path | var-name}
```

and the reader (`read_rotation_state_at`, `rotation.rs:1467-1472`) reconstructs
only those two:

```rust
let new_source = match kind.as_deref() {
    Some("file") => KeyProviderConfig::File { path: value.into() },
    Some("env")  => KeyProviderConfig::Env { variable: value },
    Some(other)  => return Err(corrupt(&format!("unknown new_source_kind {other:?}"))),
    None         => return Err(corrupt("missing new_source_kind")),
};
```

**That two-field format is the entire blocker.** Passphrase/KMS/Vault need
multiple fields (§2.1). File/Env fit in `kind`+`value`; the others do not.

**The rest of the resume machinery is already source-agnostic.** On resume the
code re-obtains the MEK by calling `load_mek(&ledger.new_source)`
(`rotation.rs:128-135`), which is *just* `build_provider()?.get_mek()`. It is
invoked from every resume seam:

- `install_pending_wal_generations` (pre-replay WAL keyring install) —
  `rotation.rs:1614`
- `finalize_resumed_wal_rotation` / index / cold resume —
  `rotation.rs:2247, 2299, 2483, 2583`
- enable resume cipher builders — `rotation.rs:1747, 1790, 1801`
- `cancel_pending_rotation` — `rotation.rs:663, 668`

So if `read_rotation_state_at` could reconstruct a `Kms`/`Vault`/`PassphraseFile`
config, **`build_provider().get_mek()` would already re-fetch/re-derive the MEK
on resume with zero further changes** — KMS `Decrypt` over the network, Vault
GET over the network, or passphrase KDF-unwrap of the on-disk `AEKF` file. This
is the key finding: the fix is a *serialization* change, not a re-architecture.

### 2.4 Is there a key hierarchy (MEK wraps DEKs) to leverage?

Yes, but it is a **derivation** hierarchy, not a local wrapping hierarchy.
`MekKeyset::derive` (`rotation.rs:318-333`) HKDF-derives four domain-separated
DEKs (wal/index/cold/checkpoint) from the MEK via `KeyDerivation`. The MEK
itself is obtained *from the provider*; it is never stored wrapped **by
AletheiaDB** on the local disk. The only "wrapped MEK" artifacts are the ones
the providers own: the passphrase `AEKF` file *is* a passphrase-wrapped MEK, and
KMS `encrypted_data_key` *is* a CMK-wrapped MEK. Vault stores the MEK
server-side. This matters for Approach B (§4.2): "wrap the MEK under the new
source" is, for KMS/passphrase, *already done* by the provider — persisting the
config reference already persists the wrapped blob (KMS) or a pointer to it
(passphrase file).

### 2.5 Serde availability (enables the simplest implementation)

`Cargo.toml:388` — `encryption = ["serde"]`. The encryption feature **always**
pulls serde, and `KeyProviderConfig` already derives `Serialize`/`Deserialize`
with `#[serde(tag = "type", rename_all = "snake_case")]`
(`config.rs:88-90`). `serde_json` is a **non-optional** dependency
(`Cargo.toml:124`). Therefore the ledger can serialize the *whole*
`KeyProviderConfig` losslessly with no new dependency and no new hand-rolled
per-field parser.

### 2.6 Engine / CLI / MCP surface

- **Engine (Rust):** `AletheiaDB::rotate_index_keys(new_source)` →
  `run_rotation` (`rotation.rs:751`); `enable_encryption` / `disable_encryption`.
  This is where the fix lives.
- **CLI:** `aletheia keys rotate --new-key <PATH> | --new-env-var <NAME>`
  (`src/bin/aletheia.rs:637-716`). Only file/env flags exist today; secret-backed
  rotation needs new flags (e.g. `--new-kms-key-id`, `--new-vault-path`,
  `--new-passphrase-file`) — a follow-up surface, not required for the engine AC.
- **MCP:** no rotation tool exists today; out of scope.

---

## 3. Problem statement (the crux, restated)

Rotation/enable/disable persist a crash-resume breadcrumb whose new-key **source
reference** is serialized as two strings. Passphrase/KMS/Vault configs do not fit
two strings, so all four call sites refuse them fail-closed. The refusal was a
*conservative placeholder* (#3602 finding G), **not** a fundamental limitation:
the configs contain no plaintext secrets, and the resume path already re-derives
the MEK generically via `build_provider().get_mek()`. The real requirement is to
serialize enough of the config to reconstruct it on resume, **while keeping the
"no plaintext secret at rest" property**, and to define the resume UX for the
sources that need an operator-supplied secret (passphrase, Vault token).

---

## 4. Approaches

### Approach A — Persist the full (non-secret) key-source reference; re-fetch/re-derive on resume

Extend the ledger encoding from two strings to the whole `KeyProviderConfig`.
Simplest and most faithful implementation: serialize `new_source` with
`serde_json` onto a single `new_source_json=<base64>` line (base64 keeps it a
single, newline-safe token in the existing line-based format), and bump the
ledger to `version=3`. The reader deserializes it back to a full
`KeyProviderConfig`. **Everything downstream is unchanged** because resume
already funnels through `load_mek → build_provider → get_mek` (§2.3).

Backward/forward compat: keep reading `new_source_kind`/`new_source_value`
(v1/v2). Prefer `new_source_json` when present. A v3 ledger without serde
compiled is impossible (encryption ⇒ serde).

**Per-source crash-resume behaviour:**

- **KMS** — resume reconstructs `Kms { key_id, encrypted_data_key, region,
  endpoint_url }` and `build_provider()` performs a fresh `Decrypt`. Fully
  external, re-fetchable, needs no operator input beyond ambient AWS creds.
  ✅ Clean.
- **Vault** — resume reconstructs `Vault { address, token_env, … }`;
  `build_provider()` reads the token from `$token_env` and GETs the secret.
  Requires the operator to have `$token_env` set in the resuming process's
  environment — **identical to the requirement to `open()` this DB normally**.
  ✅ Clean, with an operator-env precondition that already exists steady-state.
- **Passphrase** — resume reconstructs `PassphraseFile { path, passphrase_env }`;
  `build_provider()` reads the passphrase from `$passphrase_env` and KDF-unwraps
  the `AEKF` file. Requires the operator to have `$passphrase_env` set at reopen
  — again **identical to the steady-state open() requirement**. ✅ Clean, same
  precondition.

**Security:** what lands on disk is exactly today's config minus real secrets:
file paths, env-var **names**, a KMS key id + already-wrapped blob, a Vault
address/mount/path. **No plaintext secret at rest.** (One nuance: the KMS
`encrypted_data_key` — a wrapped blob — would now sit in `rotation.state`. It is
already redacted from `Debug` and is useless without the CMK, but it *is* newly
present in a second on-disk file. Flag for sign-off; mitigated by the same 0600
posture as the auth key store if desired.)

**Complexity / blast radius:** small and localized. Touches only
`write_ledger`/`read_rotation_state_at` (ledger ser/deser), and removes the four
refusal guards. No change to any resume seam, cipher, or WAL path. The existing
crash-point test matrix (#3617/#3700/#3718) re-runs unchanged against the new
sources.

### Approach B — Wrap the new MEK under the new source; store the wrapped blob in the ledger/sidecar

Generate/obtain the new MEK once at rotation start, wrap it under the new source,
and persist the *wrapped blob* durably so resume can unwrap **without
re-contacting** the source.

- **KMS** — the wrapped blob is literally `encrypted_data_key`; persisting it is
  a subset of Approach A. No benefit over A except skipping the resume-time
  `Decrypt` — but you still need the CMK to unwrap, so you re-contact KMS anyway.
  Net: no advantage.
- **Passphrase** — the wrapped blob is the `AEKF` file, which already exists on
  disk at `path`. Persisting the path (Approach A) already covers it.
- **Vault** — the only case where B differs: Vault stores the MEK server-side, so
  to "unwrap locally without re-contacting Vault" you would have to write MEK
  material (wrapped under *what* key?) to local disk. That reintroduces a local
  wrapping key = **a new secret-at-rest problem**, exactly what the refusal was
  protecting against.

**Verdict:** B is either redundant with A (KMS/passphrase) or actively worse for
security (Vault). Its only real capability — resume without touching the network
— is not an AC and not worth a new local key-wrapping scheme.

### Approach C — Hybrid: reference-persist (A) + a MEK key-check value (KCV) for clean wrong-secret detection

Approach A, plus store a small **key-check value** in the ledger — e.g.
`kcv = first 8 bytes of HMAC/AEAD-of-a-fixed-constant under the new MEK` (a
standard, non-secret KCV; reveals nothing about the key). On resume, after
`get_mek()`, recompute the KCV and compare. This converts a wrong-secret resume
(wrong passphrase, rotated-away Vault value) from a **cryptic downstream AEAD
"authenticated decrypt failed"** into a **precise "the key from this source does
not match the key this rotation was started with — check `$passphrase_env`"**.

- Crash-resume behaviour per source is identical to A.
- Security: a KCV over a fixed constant is standard KMS/HSM practice and leaks no
  key material; it is *additive* to A.
- Complexity: marginally more than A (one derive + compare on resume, one field
  in the ledger). Bounded blast radius.

---

## 5. Recommendation

**Adopt Approach A as the mechanism, with Approach C's KCV as a strongly-suggested
add-on**, and lift all four refusals (rotation + enable + disable) together since
they share the ledger primitive.

Rationale:

1. **It matches the real gap.** The only thing missing is lossless config
   serialization; the resume path is already source-agnostic (§2.3). A is the
   minimal change that satisfies AC1–AC4.
2. **No plaintext secret at rest** (AC3) — the configs carry names/wrapped-blobs,
   not secrets. This is *provable by inspection* of the five variants (§2.1).
3. **Passphrase/Vault resume needs the operator's secret env var — but that is
   the same precondition as opening the DB at all**, so it adds no new operational
   burden and is honestly documentable.
4. **Lowest blast radius.** Ledger ser/deser + guard removal; every crash-point
   test re-runs against the new sources for free.
5. The **KCV** turns the nastiest UX failure (wrong passphrase at 3 a.m. during
   crash recovery) into an actionable message, at trivial cost.

**Reject B**: redundant for KMS/passphrase, security-regressive for Vault.

---

## 6. Decisions needing sign-off before implementation

1. **Scope:** fix only `run_rotation`, or lift enable (#3700) + disable (#3718)
   refusals in the same change via the shared ledger primitive? (Recommend the
   latter.)
2. **Passphrase/Vault resume UX:** confirm it is acceptable that an interrupted
   rotation to a passphrase/Vault source **requires the operator to have the
   secret env var present at the resuming `open()`** (same as steady-state open),
   and that a missing/wrong secret is a **loud, resumable** startup failure
   (ledger retained), not a silent skip. This is the single biggest product
   decision.
3. **KMS wrapped blob at rest in `rotation.state`:** OK to write the (already
   redacted, CMK-useless) `encrypted_data_key` into the ledger file? Should
   `rotation.state` adopt 0600 perms like the auth key store?
4. **KCV yes/no** (Approach C add-on).
5. **CLI surface** for starting secret-backed rotations (`aletheia keys rotate`
   new flags) — same change or a follow-up?

---

## 7. Risks / edge-cases as the red-phase test list

Each bullet is a proposed failing test to write first. `T-KMS`/`T-VLT`/`T-PP`
denote the source under test.

**Ledger round-trip**
1. `ledger_v3_roundtrips_kms_config` — write a `Kms` new_source, read it back
   byte-for-byte equal (all four fields), `version=3`.
2. `ledger_v3_roundtrips_vault_config` — same for all seven `Vault` fields incl.
   `namespace: None` and a `ca_cert` path.
3. `ledger_v3_roundtrips_passphrase_config` — `path` + `passphrase_env`.
4. `ledger_v3_never_contains_plaintext_secret` — set `$passphrase_env` /
   `$token_env` to a sentinel; assert the sentinel never appears in
   `rotation.state` bytes (only the var *name* does).
5. `ledger_v1_v2_still_parse` — legacy `new_source_kind`/`new_source_value`
   file/env ledgers still resume (no regression to #488/#3617 DBs).
6. `ledger_v3_corrupt_json_fails_closed` — truncated/garbled `new_source_json`
   → `InconsistentState`, not "no rotation".

**Crash-resume happy paths (per source)**
7. `T-KMS crash_mid_rotation_then_resume_refetches_via_decrypt` — kill after
   ledger write + partial index roll; reopen re-`Decrypt`s and finishes; DB
   reads under the new key. (Use the `aws-smithy-mocks` injector from #3602.)
8. `T-VLT crash_mid_rotation_then_resume_refetches_via_get` — reopen re-GETs the
   secret (token from env) and finishes.
9. `T-PP crash_mid_rotation_then_resume_reunwraps_aekf` — reopen KDF-unwraps the
   on-disk `AEKF` (passphrase from env) and finishes.
10. `crash_at_each_layer_boundary_resumes` — parametrized over the existing
    #3617/#3700 crash points (C0–C6, wal_retire) but with a secret-backed source,
    proving the layer ledger drives resume unchanged.

**Loud-and-resumable failure paths (AC4)**
11. `T-KMS unreachable_at_resume_fails_loud_keeps_ledger` — endpoint down at
    reopen → startup errors, `rotation.state` still present, a later reopen with
    the endpoint up completes.
12. `T-VLT token_env_missing_at_resume_fails_loud_keeps_ledger`.
13. `T-PP passphrase_env_missing_at_resume_fails_loud_keeps_ledger`.
14. `T-PP wrong_passphrase_at_resume_is_actionable_not_cryptic` — with the KCV
    (Approach C) this is a precise key-mismatch error; without it, assert it is at
    least a clean AEAD failure that leaves the ledger intact (never a wedge).
15. `T-VLT secret_changed_in_vault_between_start_and_resume` — the Vault value
    was rotated out-of-band; resume derives a different MEK → KCV mismatch (or
    AEAD failure) → loud, ledger retained. (Documents that Vault/passphrase
    sources must be *stable* across a rotation.)

**Direction / cross-engine coverage**
16. `enable_to_kms_source_succeeds_and_resumes` — #3700 refusal lifted.
17. `disable_from_passphrase_source_succeeds_and_resumes` — #3718 refusal lifted
    (records the *current* secret-backed source to rebuild the decrypt cipher).
18. `rotate_from_secret_backed_back_to_file` — old source = KMS, new source =
    File; both `enc_cfg.key_provider` (old) and `ledger.new_source` (new) resolve;
    same-MEK `ct_eq` refusal still fires if they derive the same key.
19. `cancel_pending_rotation_to_kms_rolls_back` — an interrupted forward rotation
    to KMS, then `--cancel`, rolls every migrated file back to the old key using
    the old source.

**Guard-removal regression**
20. `feature_gated_source_without_feature_still_refuses_cleanly` — `Kms` new
    source with `encryption-aws-kms` **not** compiled → `build_provider`'s
    `Unavailable` surfaces as a clean pre-flight error, not a panic, and writes no
    ledger.

---

## 8. Implementation sketch (for the chosen approach A+C, once signed off)

- `rotation.rs`: add `new_source_json=<base64(serde_json(KeyProviderConfig))>`
  writing at `version=3` in `write_ledger`; teach `read_rotation_state_at` to
  prefer it and fall back to v1/v2 `kind`/`value`. Optionally add
  `mek_kcv=<hex8>`.
- Delete `refuse_unsupported_new_source` (or narrow it to only the
  `build_provider` `Unavailable` feature-gate case) and the `write_ledger`
  secret-backed arm.
- Remove the enable (`encryption_enable.rs:250-260`) and disable
  (`encryption_disable.rs:205-217`) match guards.
- No changes to `load_mek`, the resume seams, ciphers, or WAL — they already
  handle every provider.
- Docs: `docs/ENCRYPTION.md` (drop the "fail-closed rotation refusal" note; add
  the passphrase/Vault resume-env precondition), `docs/adr/0028`.
