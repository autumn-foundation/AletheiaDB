# Encryption — End-to-End Guide

This guide is the **navigational overview** of AletheiaDB's encryption story. It
ties together pieces that are documented in depth elsewhere — encryption at
rest, key management and providers, key rotation, enabling encryption on an
existing database, and GDPR crypto-shred — and points you to the right detailed
doc for each. It does not duplicate those docs.

## The story in one paragraph

AletheiaDB encrypts **all persisted data** — WAL segments, index files, cold
storage, and checkpoints — under a key hierarchy rooted in a master key. That
master key can live in a file, an environment variable, a passphrase-wrapped
file, **AWS KMS**, or **HashiCorp Vault**. Keys can be **rotated** without
re-encrypting bulk data, encryption can be **enabled on a database that started
out plaintext**, and individual data subjects can be **crypto-shredded** (erased
by destroying their per-subject key) to satisfy the GDPR right to erasure over
otherwise-append-only history.

## Feature flags

Encryption is **off by default** and behind feature flags:

| Flag | Enables |
|------|---------|
| `encryption` | Encryption at rest and the file / env / passphrase key providers |
| `encryption-aws-kms` | AWS KMS master-key provider (implies `encryption`) |
| `encryption-vault` | HashiCorp Vault master-key provider (implies `encryption`) |

```toml
aletheiadb = { version = "0.2", features = ["encryption-aws-kms"] }
```

## Which doc do I need?

| I want to… | Read |
|------------|------|
| Understand what's encrypted and turn it on | [ENCRYPTION.md](../ENCRYPTION.md) |
| Understand the design decision & threat model | [ADR-0028: Encryption at Rest](../adr/0028-encryption-at-rest.md) |
| Pick a key provider (file / env / passphrase / KMS / Vault) | [ENCRYPTION.md — Key Management](../ENCRYPTION.md#key-management) |
| Rotate keys | [ENCRYPTION.md — Key Rotation](../ENCRYPTION.md#key-rotation) |
| Enable encryption on an existing plaintext database | [ENCRYPTION.md — Enabling Over an Existing Dataset](../ENCRYPTION.md#enabling-encryption-over-an-existing-dataset-lazy-migration) |
| Check status / verify from the CLI | [ENCRYPTION.md — CLI Operator Commands](../ENCRYPTION.md#cli-operator-commands-issue-490) |
| Erase a data subject (GDPR right-to-erasure) | [crypto-shred.md](crypto-shred.md) |
| Export a tamper-evident audit trail | [audit-export.md](audit-export.md) |

## The building blocks

### 1. Encryption at rest

Every persistence layer is encrypted whole-file behind a small plaintext
detection header: WAL payloads (headers used as AAD), index files (including the
native HNSW graph file and its mappings sidecar), cold storage (Redb), and
checkpoints. See **[ENCRYPTION.md](../ENCRYPTION.md)** for the layer-by-layer
breakdown and **[ADR-0028](../adr/0028-encryption-at-rest.md)** for the key
hierarchy and threat model.

### 2. Key management & providers

The master key can be sourced from a file, an environment variable, a
passphrase-wrapped file, AWS KMS, or HashiCorp Vault. The KMS and Vault
providers are behind their own feature flags (above). See
**[ENCRYPTION.md — Key Management](../ENCRYPTION.md#key-management)**.

### 3. Key rotation

Rotation re-wraps data-encryption keys under a new master key without
re-encrypting bulk ciphertext, and the `keys rotate` CLI command drives index
key rotation. See **[ENCRYPTION.md — Key Rotation](../ENCRYPTION.md#key-rotation)**.
Durable, secret-backed rotation sources are covered by the design plan
[../plans/2026-07-18-durable-rotation-secret-backed-sources.md](../plans/2026-07-18-durable-rotation-secret-backed-sources.md).

### 4. Enabling encryption on an existing database (hot-live enable)

A database that was created **plaintext** can be migrated to encrypted-at-rest.
The migration engine (`src/db/encryption_enable.rs`) quiesces the background
index-persistence worker, migrates every at-rest layer (WAL, index +
checkpoint, cold) under a crash-resumable rotation ledger, then flips a durable
`encryption.state` authority so the transition completes crash-consistently. See
the design plan
[../plans/2026-07-20-hot-live-encryption-enable-driver.md](../plans/2026-07-20-hot-live-encryption-enable-driver.md)
and the lazy-migration section of
[ENCRYPTION.md](../ENCRYPTION.md#enabling-encryption-over-an-existing-dataset-lazy-migration).

### 5. GDPR crypto-shred

Because AletheiaDB is append-only and bi-temporal, you cannot physically delete
history without breaking temporal invariants and the provenance hash chain.
**Crypto-shred** encrypts the erasable payload under a **per-subject key** and
erases by destroying that key — the ciphertext may remain across every tier, but
the payload becomes permanently undecryptable. Designation and erasure are
**admin-only**, irreversible operations that return a signed attestation. See
**[crypto-shred.md](crypto-shred.md)** for the CLI/MCP surface and the *honest
limits* of what it does and does not erase.

## See also

- [ENCRYPTION.md](../ENCRYPTION.md) — the detailed at-rest reference
- [ADR-0028: Encryption at Rest](../adr/0028-encryption-at-rest.md)
- [crypto-shred.md](crypto-shred.md) — GDPR right-to-erasure
- [audit-export.md](audit-export.md) — tamper-evident audit trail export
- [security-quickstart.md](security-quickstart.md) — authentication & RBAC roles
- [provenance-hash-chain.md](provenance-hash-chain.md) — tamper-evident provenance
