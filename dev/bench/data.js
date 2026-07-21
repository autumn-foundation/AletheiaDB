{
  "lastUpdate": 1784654754208,
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
          "id": "15a20bb087205cec1ac26b70ffc04c30cfb1fe0a",
          "message": "docs: link encryption narrative guide from CLAUDE.md (#3771)\n\nAdds a single cross-reference in the \"Encryption & Compliance\" section\nof the root `CLAUDE.md`, pointing to `docs/guides/encryption.md` as the\n\"start here\" end-to-end encryption narrative that ties the existing\nencryption docs together.\n\nDocs-only, no code changes. `git diff --name-only origin/trunk` is\nexactly `CLAUDE.md` (1 line changed).\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-21T12:12:50-05:00",
          "tree_id": "ab7c81ff20a4a86c3bcf22472ad33c082de94b75",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/15a20bb087205cec1ac26b70ffc04c30cfb1fe0a"
        },
        "date": 1784654754207,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 161.13380645013797,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 19.2667813568427,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 492078.8252815114,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 196.21144786393813,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 188.1581095445008,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 195.6011516156052,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}