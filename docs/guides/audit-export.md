# Signed Audit Export (Issue #3358)

> Turn AletheiaDB's storage-layer trust into portable, courtroom-shaped evidence.

When an auditor, regulator, or opposing counsel asks *"prove what your system knew
about entity X, and when"*, a mutable JSON dump of `get_node_history` is worthless
the moment it leaves the database. A **signed audit export** is a single, self-
contained file that a third party can verify **offline** — with no AletheiaDB
instance, no network, and no trust in the operator — establishing that:

1. **Integrity** — no content in the artifact was altered after export.
2. **Completeness** — no version was added, removed, or reordered.
3. **Authenticity** — it was signed by the holder of a specific private key.

This is the evidence packet that compliance workflows actually consume: AI-governance
reviews, GDPR/CCPA subject-access requests, financial audits, and legal discovery.

## Contents

- [Quick start (CLI)](#quick-start-cli)
- [What's in the artifact](#whats-in-the-artifact)
- [The verification contract](#the-verification-contract)
- [Offline verification](#offline-verification)
- [Scope options](#scope-options)
- [Redaction](#redaction)
- [Rust API](#rust-api)
- [MCP tool](#mcp-tool)
- [Key management](#key-management)
- [Why Ed25519 (not an HMAC)](#why-ed25519-not-an-hmac)
- [Known limitation: delete/retract attribution (#3427)](#known-limitation-deleteretract-attribution-3427)
- [Independently implementing a verifier](#independently-implementing-a-verifier)

## Quick start (CLI)

```bash
# 1. Generate an operator signing key (written 0600). Prints the PUBLIC key —
#    distribute that to auditors; keep the key file secret.
aletheia audit-keygen ./audit-signing.key
# {"ok":true,"key_file":"./audit-signing.key","public_key":"f80c…6215"}

# 2. Export an entity's full bi-temporal history into a signed artifact.
#    (Opens the database via ALETHEIADB_CONFIG or ALETHEIADB_DATA_DIR.)
aletheia audit-export node 42 \
    --key ./audit-signing.key \
    --out ./evidence-node-42.audit.json \
    --db-id prod-cluster-1 \
    --redact ssn,dob

# 3. Verify OFFLINE with only the public key — no database, no network.
aletheia audit-verify ./evidence-node-42.audit.json \
    --public-key f80c55f0087b9f32f17d5ad89003a4b0fb68cc2812de7508c401ebd2ba736215
# PASS [trusted-key] db=prod-cluster-1 entities=1 versions=4 span=… root=1a12fb0d…

# 4. Render a human-readable chronology for the humans in the audit.
aletheia audit-render ./evidence-node-42.audit.json
```

`audit-verify` exits non-zero if verification fails, so it drops straight into CI.

## What's in the artifact

The artifact is a single JSON file. Top-level shape (`format_version: 1`):

| Field | Purpose |
|-------|---------|
| `metadata` | Database identity, export time, tool version, **scope** (what was/wasn't included), **chain anchor** (WAL LSN at export time), redacted keys, entity/version counts, algorithm identifiers. |
| `entities[]` | Each exported node/edge with its **complete** version list (oldest first), including superseded versions and delete tombstones. Each version carries bi-temporal coordinates, provenance (source / confidence / note / correlation_id / **principal**), and properties. |
| `chain` | The integrity proof: a per-version `leaves[]` array of SHA-256 hashes and the folded `root`. |
| `signature` | The Ed25519 `public_key`, the detached `signature` over `chain.root`, and a description of what was signed. |

Every version records `valid_from`/`valid_to` and `transaction_from`/`transaction_to`
(microseconds + HLC logical counter; open-ended bounds are explicit `null`), plus
`is_current`.

## The verification contract

The signature is **not** computed over the JSON text. It is computed over a
deterministic *canonical binary encoding* of the typed content, so that:

- Re-pretty-printing, reordering object keys, or reformatting floats **does not**
  break a valid signature.
- Changing any signed value, timestamp, provenance field, entity endpoint, or the
  order/number of versions **always** breaks it.

Construction:

1. **Leaf** — for each version, `leaf = SHA256("aletheiadb.audit.v1.leaf\0" ||
   canon(version))`, where `canon` is the domain-separated, length-prefixed binary
   encoding described [below](#independently-implementing-a-verifier). Property keys
   are sorted; floats are hashed by their IEEE-754 bits.
2. **Root** — fold, in order:
   `acc = SHA256("aletheiadb.audit.v1.root\0" || canon(metadata))`; then for each
   entity, `acc = SHA256(acc || SHA256("…header\0" || canon(header)))`, and for each
   of its versions `acc = SHA256(acc || leaf)`. The final `acc` is `chain.root`.
   The fold is **order- and count-dependent**, which is how reordering and truncation
   are detected.
3. **Signature** — `Ed25519.sign(private_key, chain.root)`.

Verification recomputes the leaves and root from the content, checks them against the
stored values, cross-checks the declared counts, and verifies the Ed25519 signature
over the recomputed root. Descriptive/algorithm labels not folded into the root are
pinned to their known constants, and unknown JSON fields are rejected — so no byte
of the artifact escapes detection.

### Chain anchor and #3351

`metadata.chain_anchor.source_lsn` records the WAL log-sequence number at export time
— the "chain-head position". When the global tamper-evident hash chain (#3351) lands,
the per-version leaves can additionally be anchored to that chain's head; the format
is designed to accommodate that additively.

## Offline verification

`aletheia audit-verify <file> [--public-key HEX]`:

- With `--public-key`, the signature is checked against that **trusted** key **and**
  the key embedded in the artifact must equal it — this defeats public-key
  substitution (an attacker re-signing with their own key and swapping the embedded
  key will fail against the trusted key).
- Without `--public-key`, the embedded key is used and the result is marked
  *self-asserted* with a prominent note — trust is only as good as the embedded key,
  which is why an auditor should always supply the signer's known public key.

Verification requires **no database and no network**. The Rust entry point is
`aletheiadb::audit::verify_json_bytes(bytes, Some(&public_key))`.

## Scope options

Each option records in `metadata.scope` exactly what was requested, what was
`included`, and what was `not_included` (with a reason) — so absence of data is never
ambiguous.

| Scope | CLI / API | Meaning |
|-------|-----------|---------|
| Single entity | `audit-export node <id>` / `AuditScope::node(id)` | One node or edge. |
| Entity set | `AuditScope::EntitySet(vec![…])` | An explicit id list; missing entities are listed under `not_included`. |
| Neighborhood | `AuditScope::Neighborhood { start, hops, valid_time, transaction_time }` | A node plus its N-hop neighborhood **at a stated bi-temporal coordinate**; the coordinate and hop radius are recorded, and only nodes/edges valid at that coordinate are included. Each member's *complete* history is exported. |

## Redaction

Redaction is **explicit and verifiable**, never silent. Redacted property keys
(`--redact k1,k2` / `ExportOptions::redact([...])`) have their **values omitted** but
the key and a `redacted: true` marker remain, and the redaction is folded into the
signature. `metadata.redacted_keys` lists every redacted key. Verification still
passes, but the redaction is visible in the artifact and in the verification report.

## Rust API

```rust
use aletheiadb::AletheiaDB;
use aletheiadb::audit::{AuditScope, AuditSigningKey, ExportOptions, verify_json_bytes};

let db = AletheiaDB::open("./data")?;
let key = AuditSigningKey::from_file("./audit-signing.key")?; // or ::generate()

let export = db.audit_export(
    AuditScope::node(node_id),
    &key,
    &ExportOptions::new("prod-cluster-1").redact(["ssn"]),
)?;
let bytes = export.to_json_bytes()?;      // the single-file artifact
std::fs::write("evidence.audit.json", &bytes)?;

// Offline verification (elsewhere, with only the public key):
let report = verify_json_bytes(&bytes, Some(&key.public_key()))?;
assert!(report.passed);
println!("{}", report.summary());
println!("{}", export.render_chronology());
```

## MCP tool

The `audit_export` MCP tool lets an LLM/agent produce an evidence artifact in one
call. Arguments: `entity_type` (`"node"`|`"edge"`), `entity_id`, optional
`database_id`, optional `redact_keys`. It returns the artifact JSON plus `public_key`,
`chain_root`, and entity/version counts. It is classified **read** in the
[access-control matrix](access-control-matrix.md) (it reads history and signs it; it
never mutates). The Ed25519 signing key is operator-provided out of band via the
`ALETHEIADB_AUDIT_SIGNING_KEY` environment variable (a 32-byte hex seed); a missing
key is a `FAILED_PRECONDITION` — the server never emits a silent unsigned export, and
the secret is never returned or logged.

## Key management

Signing keys are operator-provided, file/env based to start (AC8), and follow the
same handling conventions as the #3350 API-key store:

- The private seed is held in a zeroizing buffer, **never logged**, and the
  `Debug` impl prints only the public-key fingerprint.
- `audit-keygen` writes the key file with `0600` permissions on unix from creation.
- `AuditSigningKey::from_file` / `::from_env` / `::from_seed_hex` load a 32-byte hex
  seed; `ALETHEIADB_AUDIT_SIGNING_KEY` is the standard env variable.

**Interplay with the encryption cluster (#477–#491).** Audit signing keys are a
distinct concern from the at-rest encryption key hierarchy: encryption keys protect
data confidentiality on disk, while the audit signing key attests to the authenticity
of an exported artifact. They are configured independently. An operator who manages
encryption keys through a KMS/Vault provider can store the audit signing seed in the
same secret manager and supply it via `ALETHEIADB_AUDIT_SIGNING_KEY`; this is a
configuration choice, not a second key system baked into AletheiaDB. Rotating the
audit signing key does not affect previously issued artifacts — each artifact embeds
the public key it was signed with, and an auditor verifies against the public key that
was current when the artifact was issued.

## Why Ed25519 (not an HMAC)

Verification must be possible for a party who holds **only the signer's public key**.
A symmetric MAC (HMAC) would require the verifier to hold the same secret the signer
holds — so the operator could forge artifacts and a third party could not
independently trust them. Ed25519 detached signatures give offline, public-key-only
verification; only the private-key holder can produce a valid signature, and RFC 8032
determinism makes signatures reproducible.

## Known limitation: delete/retract attribution (#3427)

Delete and retract operations do **not** yet stamp an authenticated principal into
version provenance (tracked as Issue #3427: destructive-op attribution needs a WAL
payload extension to survive crash recovery). The audit export surfaces provenance
**faithfully — including its absence** on delete tombstones — and never fabricates
attribution. In a rendered chronology a delete tombstone shows
`Provenance: (none recorded)`. This is documented honestly here rather than papered
over; when #3427 lands, delete/retract versions will carry a principal like any other
write, with no format change required.

## Independently implementing a verifier

A third party can implement a verifier from this section alone (plus SHA-256 and
Ed25519 libraries). Canonical primitive framing — **all integers big-endian**:

- `u8` tag; `u32`/`u64`/`i64` fixed-width; `f64` as its IEEE-754 `to_bits()` `u64`.
- `bool` as one byte (`0`/`1`).
- Length-prefixed byte strings: a `u64` length followed by the raw UTF-8/bytes.
- `Option<T>`: a `0` byte for absent, or a `1` byte followed by `T`.

**Version leaf** `canon(version)` writes, in order: entity kind (str), entity id
(`u64`), version_number (`u64`), version_id (`u64`), label (str), valid_from micros
(`i64`) + logical (`u32`), valid_to (opt `i64`) + logical (opt `u32`), transaction_from
micros (`i64`) + logical (`u32`), transaction_to (opt `i64`) + logical (opt `u32`),
is_current (bool), provenance (opt: source/confidence/note/correlation_id/principal,
each an `Option`), then the property count (`u64`) followed by each property **sorted
by key**: key (str), redacted (bool), value (opt, tagged by type — `0` null, `1` bool,
`2` i64, `3` f64-bits, `4` string, `5` bytes, `6` array, `7` dense vector of f64-bits,
`8` sparse vector). `leaf = SHA256("aletheiadb.audit.v1.leaf\0" || canon(version))`.

**Entity header** `canon(header)` writes kind (str), id (`u64`), label (opt str),
source (opt `u64`), target (opt `u64`);
`header_hash = SHA256("aletheiadb.audit.v1.header\0" || canon(header))`.

**Metadata** `canon(metadata)` writes database_id, exported_at, tool_version,
scope_description, scope.kind, then the requested/included/not_included lists, the
optional coordinate, hops (opt `u32`), chain_anchor.source_lsn (`u64`) + description,
the **sorted** redacted keys, entity_count (`u64`), version_count (`u64`), and the two
algorithm strings.

**Root** — `acc = SHA256("aletheiadb.audit.v1.root\0" || canon(metadata))`; then for
each entity `acc = SHA256(acc || header_hash)` and for each of its versions
`acc = SHA256(acc || leaf)`. The final `acc` must equal `chain.root`.

**Signature** — verify the 64-byte Ed25519 `signature` over the 32-byte root using the
32-byte `public_key`. Also confirm `format_version == 1`, the algorithm labels, and
the declared entity/version counts. Reject any unknown JSON fields.
