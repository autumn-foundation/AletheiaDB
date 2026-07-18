# Plan: Fold crypto-shred keyring + designation registry into `.albk` (format v7)

Issue #3665 — Slice 3, Shape B. Direction user-approved. Companion to the
crypto-shred roadmap [`2026-07-16-gdpr-crypto-shred.md`](./2026-07-16-gdpr-crypto-shred.md).

## Problem

The crypto-shred subject keyring (`subject_keyring.dat`) **is** the designation
registry: it holds each subject's wrapped DEK, its designation targets, its
lifecycle state (`Active`/`Erased`), `erased_at`, and the signed erasure
attestation. Before this change the `.albk` backup captured graph + temporal +
constraints but **not** the keyring. So a backup→restore over a database that
used crypto-shred came back **over-erased**: every designated property was a
SUBJ-sealed envelope with no per-subject DEK to unseal it, i.e. active subjects'
data was un-recoverable even though it should have been readable.

## Goal

The keyring travels inside the v7 archive so that after restore:

- ACTIVE subjects' designated properties are readable again (their wrapped DEK
  rides along, encrypted under the MEK).
- ERASED subjects stay erased — their wrapped DEK is `None` in the exported
  sidecar, so its ciphertext is physically absent from the archive.
- `erased_at` + attestation survive and still verify.
- v6/v5 archives still restore (documented degradation: no keyring ⇒ designated
  properties restore sealed-unreadable, with a loud warning).

## Brainstorm — what could live in the archive

- The full keyring sidecar bytes (chosen — it is the single source of truth for
  designations + erasure state + attestations).
- Just the wrapped DEKs (rejected — loses designation targets, erased-state,
  `erased_at`, attestations; would need a second structure).
- The unwrapped DEKs (rejected outright — that would put raw key material in a
  portable artifact; catastrophic).
- A re-derivation recipe instead of the DEKs (impossible — per-subject DEKs are
  random and independent of the MEK by design; that independence is what makes
  crypto-shred irreversible).

## Reverse-brainstorm — how could this leak keys or corrupt restore?

- **Leak an erased subject's DEK.** Mitigation: export from `to_sidecar()`,
  whose erased entries carry `wrapped_key = None`; T4 byte-scans the archive to
  prove the pre-erasure wrapped-DEK ciphertext is absent.
- **Leak raw key material.** We only ever move the *wrapped* DEK (MEK-encrypted
  AEAD blob). The MEK itself is never in the archive (it stays in the operator's
  key file). Tests never print any key bytes.
- **Corrupt restore via a malformed sidecar.** The exported bytes are the exact
  `encode_sidecar_with_crc` wire form and are written verbatim; the reopen loads
  them through the fail-closed `CryptoShredState::open`, which rejects a corrupt
  file (startup aborts) rather than silently starting empty.
- **Silent over-erasure on old archives.** A v5/v6 archive has no keyring; we
  detect residual SUBJ-sealed properties and emit a loud one-time warning.
- **Feature-coupling breakage.** `BackupPayload` is always compiled but the
  crypto-shred types are behind `audit-export`. Mitigation: the new field is
  opaque `Vec<u8>`, so `BackupPayload` stays feature-independent (compiles under
  `--no-default-features`).
- **Backup/restore now key-bearing.** Documented: a v7 `.albk` must be protected
  like key material.

## Six-hats summary

- **White (facts):** keyring = designations + wrapped DEKs + erased-state +
  attestations; archive format is `[MAGIC][u16 version][zstd(bitcode payload)]`.
- **Red (gut):** moving key-adjacent bytes into a portable file feels risky —
  answer with explicit absence tests and docs.
- **Black (caution):** old archives, corrupt sidecars, feature gating, DEK leak.
- **Yellow (upside):** GDPR-correct portable backups; erasure survives transport.
- **Green (creative):** opaque-bytes field keeps the always-compiled payload
  struct clean of feature-gated types.
- **Blue (process):** mirror the exact schema-constraint fold pattern (freeze
  `BackupPayloadV6`, add a `From`, add a `read_artifact` branch, thread through
  `build_payload`, write sidecar on restore).

## Approaches considered

1. **Opaque `keyring_sidecar: Vec<u8>` on `BackupPayload` (CHOSEN).** The field
   is feature-independent; `audit-export`-gated code fills it (backup) and
   consumes it (restore). Pros: `BackupPayload` compiles with the feature off;
   mirrors the constraint-fold pattern exactly; zero new bitcode of gated types
   in the always-compiled struct. Cons: bytes are opaque at the payload layer
   (no structural validation until reopen) — acceptable, the fail-closed loader
   validates.
2. **Feature-gated typed field (`#[cfg(audit-export)] keyring: Option<SubjectKeyringSidecar>`).**
   Pros: typed. Cons: `BackupPayload` layout would differ by feature, so a
   backup taken with `audit-export` on could not even be *parsed* with it off;
   breaks the "always restorable" contract and the frozen-shape discipline.
   Rejected.
3. **Move crypto-shred sidecar types out of the `audit-export` gate.** Pros:
   lets the payload hold typed keyring data unconditionally. Cons: widens the
   always-compiled surface, drags Ed25519/audit deps toward default, larger
   blast radius than the fold warrants. Rejected.

## Risks-as-tests

- **T1** active subject's designated property readable after restore.
- **T2** erased subject stays erased; `erased_at` + attestation survive.
- **T3** the erased subject's attestation verifies after restore.
- **T4** the pre-erasure wrapped-DEK ciphertext is absent from the v7 archive.
- **T5** sentinel scan of the `.albk` finds zero designated plaintext.
- **T6** (pure format) a v6 legacy archive still restores → empty `keyring_sidecar`.
- **T7** a plain (no crypto-shred) DB round-trips; `keyring_sidecar` empty; no
  `subject_keyring.dat` written; reopen has no crypto state.

## Implementation shape

- `src/storage/backup/mod.rs`: bump `BACKUP_FORMAT_VERSION` 6→7; add
  `keyring_sidecar: Vec<u8>` to live `BackupPayload`; freeze `BackupPayloadV6` +
  `From`; add a `6 =>` branch in `read_artifact`; thread `keyring_sidecar`
  through `build_payload`.
- `src/db/crypto_shred/`: `CryptoShredState::export_sidecar_bytes()` (empty when
  no entries); `keyring::save_sidecar_bytes()` (verbatim durable write);
  `encode_sidecar_with_crc` made `pub(crate)`.
- `src/db/backup.rs`: export in `backup()`; on restore write
  `{data_dir}/subject_keyring.dat` (both ephemeral + durable) and warn on a
  keyring-less archive that still holds sealed properties.
