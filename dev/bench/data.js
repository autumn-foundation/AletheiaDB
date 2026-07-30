{
  "lastUpdate": 1785377036074,
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
          "id": "9004cee967cdc40a85692b049b4796af0b31496f",
          "message": "fix(rotation): refuse a cancel that would leave the DB split-key (#3783) (#3786)\n\nFixes #3783.\n\n## The bug\n\n`AletheiaDB::cancel_pending_rotation` drives exactly one engine — the\nindex/checkpoint `IndexKeyRotation` reverse pass. A full-MEK rotation\n(#3617)\nalso re-keys the **WAL**, the **cold (redb) tier**, and the\n**crypto-shred\nsubject keyring**, and none of those has a reverse pass. The cancel\nnevertheless\ncleared the ledger, emitted a `RotationCompleted` audit event, and\nreturned\n`Ok(RotationReport)`.\n\nSo the operator was told the rollback succeeded while the database was\nleft\n**split-key**: index/checkpoint under the OLD MEK, WAL / cold / subject\nkeyring\nunder the NEW one. Acting on that report — \"the new key was never\nadopted, I can\ndiscard it\" — makes every new-DEK WAL segment (whose old-generation\ncounterparts\nthe forward pass already retired), every re-wrapped cold value, and\nevery\nwrapped per-subject DEK **permanently undecryptable**.\n\n## The fix (direction 1 from the issue: loud refusal instead of silent\ncorruption)\n\nCancel now refuses, up front, when the pending ledger records\n`layer.wal`,\n`layer.cold`, or `layer.subject_keyring` as anything other than\n`Skipped`.\n\n- New `RotationError::UnsupportedCancelWithRekeyedLayers` (mapped to\n`Error::FailedPrecondition`) names the offending `layer=status` pairs\nand\npoints the operator at rolling **FORWARD** with `keys rotate --resume`,\nwhich\n*is* symmetric and idempotent across every layer, plus the \"keep the old\nkey,\n  do NOT discard the new key\" instruction.\n- **`Pending` blocks as well as `Complete`**, deliberately: the ledger\ncannot\n  distinguish \"this layer's pass never started\" from \"it was interrupted\nhalf-way\", and a half-rolled WAL has already had its old-generation\nsegments\n  retired. Fail closed on the ambiguity.\n- The guard runs **before** any keyring generation is installed and\n**before**\nthe cancel marker is published, so a refused cancel leaves the pending\nforward\nledger **byte-identical** and re-encrypts nothing — `--resume` still\nworks.\n- The refusal is audited as `key.rotation.failed` (key-safe category\ntoken\n  only), **never** as `key.rotation.completed`.\n- `resume_pending_rotation` re-asserts the same refusal on a `cancel`\nledger,\nchecked *first* — before the WAL block that would otherwise roll the WAL\n*forward* off a cancel ledger — and retains the ledger rather than\nclearing it.\n\nAn **index-only** rotation (every non-index layer `Skipped`) is exactly\nthe case\nthe reverse pass covers, so it still cancels normally.\n\n## Secondary residue also fixed\n\nThe cancel marker was written through `write_rotation_state`, which\nbuilt a\n`RotationLedger::index_scope` — flattening `wal` / `cold` / `wal_retire`\n/\n`subject_keyring` to `Skipped` and dropping `mek_kcv`. So the moment the\ncancel\nmarker landed, the ledger no longer recorded which layers had been in\nflight,\nand #3620 wrong-key detection was lost across a cancel.\n\nA new `RotationLedger::cancel_scope(&pending)` derives the marker\n**from** the\npending ledger: index/checkpoint reset to `Pending` (the reverse pass\nstill has\nto run), every other per-layer status **and** `mek_kcv` preserved\nverbatim.\n`index_scope` / `write_rotation_state` had no remaining production\ncaller and are\nnow `#[cfg(test)]`.\n\nThe full symmetric reverse pass per layer (direction 2 in the issue —\nmirroring\n`roll_and_retire_wal` under the old DEK, `reencrypt_cold_values` back to\nthe old\ngeneration, `rewrap_keyring` back to the old `subject-wrap` cipher) is\n**not**\nimplemented here; this change makes the gap loud and non-destructive,\nand the\nnow-preserved per-layer statuses are the state a future reverse pass\nneeds.\n\n## Tests\n\nFive new tests in `src/db/rotation.rs`:\n\n- `unrollbackable_layers_names_only_rekeyed_non_index_layers` — the\npredicate:\neach of the three layers blocks at `Pending` *and* `Complete`; `index` /\n`checkpoint` / `wal_retire` never block; all three are named together in\n  driver order.\n- `cancel_scope_preserves_non_index_layer_statuses_and_kcv` — statuses,\nKCV, and\n`rotation_old_version()` survive the direction flip and the durable\nround-trip.\n- `cancel_pending_rotation_refuses_when_a_non_index_layer_was_rekeyed` —\nover\nfive layer combinations, against a genuinely half-migrated (v1/v2) index\ntree:\n`FailedPrecondition`, the layers named, `--resume` suggested, the ledger\nbytes\nunchanged, and the per-file `key_version` map unchanged (proving the\nreverse\npass did not run). Then flattens the layers to `Skipped` and shows the\nsame\n  ledger still cancels.\n- `resume_refuses_a_cancel_ledger_recording_a_rekeyed_layer` — the\nresume-path\n  backstop; ledger retained verbatim.\n- `refused_cancel_audits_a_failure_not_a_completion` —\n`key.rotation.failed`\n  present, `key.rotation.completed` absent, raw error text absent.\n\nVerification: `cargo test --all-features --lib` → 6789 passed, 0 failed;\n`cargo test --all-features --test\n{cli_encryption,cli_keys,encryption_state_authority,index_encryption}`\n→ 56 passed, 0 failed; `cargo clippy --all-targets --all-features -- -D\nwarnings`\n→ clean; `cargo fmt --all` applied.\n\n## Docs\n\n`docs/ENCRYPTION.md` gains a `keys rotate --cancel` scope callout with\nthe\nverbatim refusal message and the roll-forward recovery procedure; the\n`rotation.rs` module doc and `cancel_pending_rotation` /\n`resume_pending_rotation`\ndoc comments record the scope and the errors.\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_017NSyZg422Uw3KhbK2GLt8n\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_017NSyZg422Uw3KhbK2GLt8n)_\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-29T20:52:57-05:00",
          "tree_id": "7287b2e738f01bb4381170e28025f36ab98bc10c",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/9004cee967cdc40a85692b049b4796af0b31496f"
        },
        "date": 1785377036073,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 489260.9733099692,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 155.82575074257724,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 176.0940307906623,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 171.04643229514386,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 182.55749004570433,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 18.99841825730384,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}