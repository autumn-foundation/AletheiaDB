{
  "lastUpdate": 1785347387621,
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
          "id": "d4201c67e79df5bf0346df8157076c664e1eea90",
          "message": "TS SDK: priority_properties on GET /schema; refresh flat-envelope fixtures (#3679) (#3781)\n\nCloses #3679.\n\nBoth halves of the issue that #3659 left open, developed red → green →\nrefactor.\n\n## 1. `priority_properties` on GET reads\n\n#3659 wired **eight** of the nine budgetable GET reads and kept\n`getSchema` excluded behind a bespoke scalar-only `schemaBudgetQuery`.\nThat exclusion was wrong: the server's `GetSchemaQuery` carries the same\n`de_priority_properties` comma-split as every other GET read\n(`crates/aletheia-server/src/schema_batch_tools.rs`), `get_schema` is in\nthe #3353 budgetable cohort, and the server has a passing parity test\nfor it (`get_schema_priority_properties_get_param_accepted_and_parity`).\nThe SDK was silently dropping a supported parameter.\n\nEnumerating the server's route table gives exactly **nine** `#[get]`\nroutes carrying `de_priority_properties`, and the SDK now covers all\nnine 1:1:\n\n| | `GET /nodes/{id}` | `GET /nodes` | `GET /edges/{id}` | `GET /edges`\n| `GET /traverse` | `GET /nodes/{id}/history` | outgoing | incoming |\n`GET /schema` |\n|---|---|---|---|---|---|---|---|---|---|\n\n`GetSchemaOptions` gains `priorityProperties` (composing the shared\n`BudgetOptions` rather than duplicating it) and `getSchema` uses the\ncommon `budgetQuery`.\n\n## 2. Flat-envelope fixture refresh\n\nThe last legacy flat `{success:false,error}` fixture — a 429 in\n`transport.test.ts` — is replaced by the nested #3234/#3629 envelope the\nserver actually emits. **That swap exposed a real defect**:\n`parseMcpError` derived its fallback `retriable` from the code alone, so\na nested 429 omitting `retriable` normalized to *non*-retriable, while\nthe flat path's status-aware default made the identical error retriable.\nThe two defaults are now one shared `defaultRetriable(code,\nhttpStatus)`.\n\nThe retained flat parse branch is no longer an unlabeled remnant: it is\nexercised through a `legacyFlatHttpError()` fixture documented as\nback-compat-only, and `test/error-envelope.test.ts` asserts both shapes\nnormalize to observably identical errors.\n\n## Review findings fixed\n\nFour independent reviews (wire-contract parity, test quality, API\nsurface/semver, adversarial correctness) found defects in the first\ncommit and in code it touched. All fixed in `e97aaff`:\n\n- **429 default was too broad** — gating on status alone made a 429\ncarrying `NOT_FOUND`/`INVALID_ARGUMENT` retriable, contradicting #3234.\nNow gated on `RESOURCE_EXHAUSTED`. The docstring's rationale was also\nwrong: the server emits 429 with explicit `retriable: false` for a\nwrite-class wall-clock timeout (the write may already have committed)\nand a tenant-quota breach.\n- **`statusToCode` disagreed with the server** — 422 is\n`InvalidLimitOverride` → `INVALID_ARGUMENT`, not a resource cap; 412\n(`FAILED_PRECONDITION`) had no case at all.\n- **Malformed bodies lost information** — a nested body with a\nmissing/non-string `code` discarded the server's `message`;\n`retriable`/`details`/`trace_id` were forwarded unvalidated (a `\"true\"`\nstring could land in a field declared `boolean`).\n- **`priorityProperties` diverged between GET and POST** — the server\ntrims/drops empties on GET but takes a POST body verbatim; `['']`\nemitted the bare `?priority_properties=` the docs promised never to\nemit. Normalization is now shared. A name containing a comma is rejected\nwith `INVALID_ARGUMENT` rather than silently protecting two properties\nthat don't exist.\n- **Tests were weaker than they looked** — mutation testing showed the\nequivalence block stayed green with `parseHttpError` gutted to a\nconstant. Every case now asserts absolute expected values, with\ncross-shape equality as a secondary check.\n\n## Verification\n\n`155 tests passed` (was 90), typecheck, lint, dual ESM+CJS build, and\nboth entry-point smokes pass. New coverage: status→code fallback across\nthe vocabulary (previously only the bare 401 was pinned), end-to-end\nretry against a real HTTP status (retry was only ever exercised\nin-band), malformed-body salvage, GET/POST normalization parity,\n`getSchema` with `asOf*` + budget merged, and budget alongside a cursor.\n\n## Notes / out of scope\n\n- `COMPATIBILITY.md` gains a rule for runtime-behavior changes, which\nits three semver categories did not cover, and the `0.2.x` row now\ndocuments the behavioral half of this change, not just the additive\nhalf.\n- **Not addressed** (pre-existing, separate concerns): the retry policy\nmakes no idempotent/non-idempotent distinction (#3369 design); the SDK\ndoes not read the `x-trace-id` or `Retry-After` response headers (needs\na `FetchResponseLike` surface change, #3376 territory); a code-less HTTP\n409 still defaults to `CONFLICT` (genuinely ambiguous server-side — 409\nis both `CONFLICT` and `CONSTRAINT_VIOLATION`).\n\n---\n_Generated by [Claude\nCode](https://claude.ai/code/session_01CdeFSxZKX6NJ1bNDAdyFLD)_\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-29T12:36:18-05:00",
          "tree_id": "3da237e89bdece995b939f0690a42fa3bf16b0ee",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/d4201c67e79df5bf0346df8157076c664e1eea90"
        },
        "date": 1785347387621,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 494526.0044443266,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 164.8726871078827,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 191.86300327477917,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 189.16385085296824,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 197.76943272748343,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.48101822303881,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}