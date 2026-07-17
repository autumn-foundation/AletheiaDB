# GDPR Crypto-Shred — Right-to-Erasure over Bi-Temporal History (Issue #3359)

AletheiaDB is an append-only, bi-temporal store: you cannot physically delete
history without breaking temporal invariants and the tamper-evident provenance
hash chain. **Crypto-shred** reconciles that with the GDPR "right to erasure"
by encrypting the erasable payload under a **per-subject key** and erasing by
**destroying that key**. The ciphertext may remain across every storage tier,
but once the key is gone the payload is **permanently undecryptable** — a
recognized crypto-shredding approach to erasure.

This guide covers how to designate an erasure subject and erase it via the
`aletheia` CLI, the signed attestation you get back, and — importantly — the
**honest limits** of what crypto-shred does and does not erase.

> Admin-only operation. Designation and erasure are privileged, irreversible
> operations. On the CLI they run in a local-admin context (the same trust
> level as `backup` or `keys rotate`). On the MCP / HTTP server surfaces they
> are gated to the **admin** role (see [security-quickstart.md](security-quickstart.md)).

## Concepts

- **Erasure subject** — a caller-designated grouping of one or more entities
  and/or specific property keys under a single **subject id**. It is the unit
  of erasure. Crypto-shred is **caller-driven**: there is no automatic PII
  discovery — you decide what belongs to a subject.
- **Designation target** — one entry in a subject's designation:
  - a **whole node** or **whole edge** (seals all of its non-reserved property
    keys), or
  - **specific property keys** of a node or edge (seals only those keys).
- **Sealed envelope** — at the single write choke point, a designated value is
  replaced with a self-describing encrypted envelope. Every tier (hot RAM,
  WAL, cold storage, checkpoints, backups) then carries only ciphertext.
- **Erasure** — destroying the subject's key material. After erasure the sealed
  envelopes can never be decrypted again, even with the master key.
- **Attestation** — a signed, content-free proof that a subject was erased.

**Prerequisite: encryption must be configured.** Crypto-shred builds its
per-subject keys from the database's encryption key provider. A database opened
without encryption cannot designate a subject — the command fails cleanly.
Configure encryption with an `ALETHEIADB_CONFIG` TOML that enables a key
provider (see [security-quickstart.md](security-quickstart.md) and the
encryption guides).

## CLI

The `aletheia` CLI exposes two verbs, available when built with the
`audit-export` feature (on by default). Both open the database via
`ALETHEIADB_CONFIG` (a TOML that can enable encryption) or
`ALETHEIADB_DATA_DIR`.

### Designate a subject

```bash
# Group two whole nodes, a whole edge, and two specific properties of a node
# under the subject id "user-42".
ALETHEIADB_CONFIG=./aletheia.toml \
  aletheia designate-subject user-42 \
    --target node:100 \
    --target node:101 \
    --target edge:200 \
    --target node:101:ssn,email
```

`--target` is repeatable; at least one is required. Each target is
`<kind>:<id>` (whole entity) or `<kind>:<id>:key1,key2` (only the listed
property keys), where `<kind>` is `node` or `edge`.

Output (single JSON line):

```json
{"ok":true,"subject_id":"user-42","targets_designated":4}
```

Designating additional targets under an existing, still-active subject merges
them. A subject id must be non-empty, at most 256 bytes, and free of control
characters.

### Erase a subject

```bash
ALETHEIADB_CONFIG=./aletheia.toml aletheia erase-subject user-42
```

Output (single JSON line) — the signed **erasure attestation**:

```json
{
  "ok": true,
  "subject_id": "user-42",
  "entity_count": 3,
  "timestamp_micros": 1752768000000000,
  "timestamp": "2026-07-17T18:40:00+00:00",
  "signature": "…128 lowercase hex chars…",
  "signer_public_key": "…hex…"
}
```

The attestation carries **only** the subject id, the number of designated
entities, a timestamp, and an Ed25519 signature over those fields — **never any
property content**. The `signer_public_key` lets an auditor verify the
signature independently. Re-erasing an already-erased subject is an idempotent
no-op that returns the same recorded attestation.

Errors are printed to stderr as `error: <message>` and exit non-zero. For
example, designating on a database without encryption configured:

```
error: encryption is not configured; crypto-shred requires an encryption key provider [FAILED_PRECONDITION]
```

## MCP / HTTP

Admin-gated `designate_subject` and `erase_subject` MCP tools (and their HTTP
equivalents) expose the same operations to server callers holding the **admin**
role. They are a follow-up to this CLI slice; see the access-control matrix and
[security-quickstart.md](security-quickstart.md) once they land.

## Honest limits

Crypto-shred is a strong, practical erasure mechanism, but it is **not** magic.
Understand these boundaries before relying on it for compliance.

1. **Forward-only sealing (late designation does not shred the past).**
   Designation seals **forward** from the moment it is applied. Property
   versions written **before** a subject was designated remain in plaintext in
   history and are **out of the erasure boundary** — crypto-shred cannot
   retroactively shred data already written in the clear without a
   history-rewriting migration. **Prefer designating a subject at creation
   time.**

2. **Pre-designation / pre-erasure backups and audit-exports are out of
   boundary (AC7).** A backup (`.albk`) or audit-export produced **before** a
   subject was designated (or before it was erased) still contains that
   subject's plaintext (or still-decryptable) payload. Erasing the live
   database does **not** reach into artifacts that were already exported.
   Operational procedure — tracking and destroying stale artifacts — is
   required for full compliance.

3. **Topology and structure are not erased.** Which entities and edges exist,
   their labels, and their temporal coordinates persist after erasure. Only the
   designated **property payload** (and designated embeddings) become
   undecryptable. This is a deliberate v1 scope decision: erasing topology would
   break referential integrity and the temporal model.

4. **Designated embeddings are not semantically searchable (v1).** A designated
   vector/embedding is excluded from the shared plaintext HNSW index so that no
   plaintext floats exist to leak. The trade-off is that such embeddings are not
   returned by `find_similar` / ANN search. Per-subject index partitions to
   restore search are a future enhancement.

5. **Key-registry media erasure is the operator's responsibility.**
   Crypto-shred reduces the erasure problem to destroying one small secret and
   rewrites the key registry atomically without it. But the actual media-level
   destruction of the old bytes depends on your filesystem and hardware.
   Wear-leveled media (SSDs) may retain copies until overwritten; use
   secure-erase-capable storage for the key registry where regulation demands
   media-level guarantees.

6. **Threat model.** If the **master key is compromised *before* erasure** and
   an attacker also captured a copy of the key registry at that time, the
   subject is exposed — that is true of any at-rest encryption. **After**
   erasure the wrapped per-subject key blob is gone, so even the master key can
   no longer recover the payload. This is the standard crypto-shred model: the
   per-subject key is random and independent, never re-derivable from the master
   key.

7. **Warm-cache visibility caveat (read-cache-eviction-bounded).** Erasure
   destroys the key and updates the durable stored envelopes immediately, and a
   fresh reopen (`verify_chain`) gives a definitive tamper/erasure verdict over
   the at-rest bytes. However, a warm **in-process** reconstruction cache built
   under the version-immutability assumption can continue to serve an
   already-materialized pre-erasure view of a cached version until that cache
   entry is evicted or the process restarts. Equivalently, an at-rest byte
   mutation of an *already-cached* version can be masked by the warm cache until
   eviction/restart. Treat erasure visibility as **read-cache-eviction-bounded**:
   for a definitive post-erasure verdict, rely on a fresh reopen rather than a
   long-lived warm process.

## See also

- [security-quickstart.md](security-quickstart.md) — authentication, RBAC
  roles, and the admin gating for crypto-shred on the server surfaces.
- [access-control-matrix.md](access-control-matrix.md) — the canonical
  role/operation authorization matrix.
- [../plans/2026-07-16-gdpr-crypto-shred.md](../plans/2026-07-16-gdpr-crypto-shred.md) —
  the full design, six-hats analysis, and the risk/test matrix.
