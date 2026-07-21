{
  "lastUpdate": 1784652157909,
  "repoUrl": "https://github.com/autumn-foundation/AletheiaDB",
  "entries": {
    "AletheiaDB Benchmarks": [
      {
        "commit": {
          "author": {
            "email": "markmasterson@gmail.com",
            "name": "Mark Masterson",
            "username": "madmax983"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "ebe0388d0b48e1f2ae7132a695ce1414279682af",
          "message": "docs: CLAUDE.md factual refresh (proposal) (#3769)\n\n## Summary\n\nA **proposal** to refresh the factual accuracy and completeness of the\nroot `CLAUDE.md`. This is a **high-blast-radius onboarding file**, so\nthe change is deliberately scoped to **factual corrections +\ncompleteness only — no structural rewrite**. Open for redirection on\nwording/placement.\n\nOnly `CLAUDE.md` changes; no code touched.\n\n## Before → After (per fix category)\n\n### A. SQL:2011 — stale \"planned\" claim\n- **Before:** `- SQL:2011 temporal syntax (planned)`\n- **After:** `- ✅ SQL:2011 temporal syntax (implemented, behind the\n`sql` feature — `parse_sql`, `FOR SYSTEM_TIME AS OF` / `FOR VALID_TIME\nAS OF`; see `src/sql/`)`\n- Verified: `src/sql/` module, `sql = [\"dep:sqlparser\"]` (Cargo.toml),\n`pub mod sql` (src/lib.rs), `FOR SYSTEM_TIME AS OF` parsing in\n`src/sql/temporal_parser.rs`.\n\n### B. Non-compiling \"LLM Integration\" examples\n- **Before:** fluent methods that do not exist —\n`db.as_of(\"iso\").find_node(...)`, `db.between(...).track_changes(...)`,\n`db.as_of(T).get(X)`, `db.history(Y).changes()`,\n`db.first_occurrence(F)`.\n- **After:** real, compiling snippets mapped to the same intent, marked\nillustrative:\n- `db.get_node_at_time(node_id, valid_time, transaction_time)` (\"what\ndid we know about X at T\")\n- `db.find_nodes_at_time(\"Person\", valid_time, transaction_time)`\n(entry-point resolution)\n  - `db.get_node_history(node_id)` (\"how has Y changed\")\n- `db.query().as_of(valid_time,\ntransaction_time).start(alice_id).traverse(\"KNOWS\").execute(&db)`\n(bi-temporal traversal — `as_of` lives on `QueryBuilder` and takes two\n`Timestamp` args)\n  - `db.execute_cypher(\"... AS OF SYSTEM_TIME ...\")`\n- Verified every signature against `src/db/temporal.rs`,\n`src/db/ops.rs`, `src/query/builder.rs`, `src/db/query.rs`.\n\n### C. Missing MCP tools (14 registered tools absent from the tables)\nVerified against `src/mcp/auth.rs` `TOOL_ACCESS_CLASSES` (CI-enforced\nregistry). Added:\n- **Edges:** `count_edges`\n- **Temporal:** `get_node_at_valid_time`,\n`get_node_at_transaction_time`, `get_edge_at_valid_time`,\n`get_edge_at_transaction_time`, `diff_edge_versions` (plus folded in\n`get_node_history`, `get_edge_history`, `diff_node_versions` for a\ncomplete row)\n- **Constraints (new row):** `enable_unique_constraint`,\n`list_unique_constraints`\n- **Namespaces (new row):** `list_namespaces`, `describe_namespace`,\n`create_namespace`\n- **Audit (new row):** `audit_export`\n- **GDPR crypto-shred (new Admin-class row):** `designate_subject`,\n`erase_subject`\n\n### D. Encryption visibility (previously zero mentions)\nAdded a concise **Encryption & Compliance** subsection under Major\nFeatures: encryption-at-rest (`encryption`, ADR-0028), KMS/Vault\nproviders (`encryption-aws-kms`, `encryption-vault`), key rotation,\nhot-live enable (`src/db/encryption_enable.rs`), and GDPR crypto-shred —\nlinking `docs/ENCRYPTION.md`, `docs/adr/0028-encryption-at-rest.md`,\n`docs/guides/crypto-shred.md`.\n\n### E. Vector Search \"Phase 5\" wording\n- **Before:** \"Phase 5 pending\" with persistence listed as unshipped.\n- **After:** notes vector index persistence has **shipped**\n(`src/storage/index_persistence/vector.rs`), keeping\nstreaming/incremental as remaining.\n\n### F. Owner references\nNo `madmax983/AletheiaDB` refs exist in `CLAUDE.md` — **no change\nneeded** (no-op).\n\n## Scope note\nThis is a **proposal** for a high-blast-radius onboarding file, scoped\nto factual/completeness fixes with **no structural rewrite**. Diff is 65\ninsertions / 13 deletions, `CLAUDE.md` only. Open for redirection.\n\n## Drift spotted but deliberately NOT touched (follow-ups)\n- **Task requested a link to `docs/guides/encryption.md` (a new\nnarrative guide) — that file does not exist on disk.** I linked only the\nexisting `docs/ENCRYPTION.md` / ADR / crypto-shred guide instead.\nAuthoring that narrative guide is a follow-up.\n- **ADR front-matter mismatch:** `docs/adr/0028-encryption-at-rest.md`\nhas an internal header reading \"ADR-0026\" — filename says 0028. Not\ntouched (code/docs outside CLAUDE.md).\n- **15 stale `version = \"0.1\"` strings** across docs + several\n`src/**/*.rs` doc-comments (crate is `0.2.0`) — out of scope for this\nCLAUDE.md-only change.\n- **Guides index (`docs/guides/README.md`) is stale** — ~22 guides\n(incl. `mcp-query-tool.md`) unlinked. Separate doc-index PR.\n- Four shipped `semantic-temporal` features (belief revision, half-life,\ncontradiction genealogy, counterfactual replay) have design docs but no\nuser guide.\n\n## Audit-vs-code discrepancy\nThe audit noted the GDPR tools were historically named\n`designate_crypto_erasable` / `crypto_erase_subject`; the **current**\nregistered names in `src/mcp/auth.rs` are `designate_subject` /\n`erase_subject`. I trusted the code and used the current names.\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-21T11:26:28-05:00",
          "tree_id": "a740025b89ff669f8098482332cb5b54501dfb3c",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/ebe0388d0b48e1f2ae7132a695ce1414279682af"
        },
        "date": 1784652157908,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 156.54349453471647,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.38687638582061,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 480933.8916731581,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 182.57704983952897,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 176.81170963938942,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 172.3791376401269,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}