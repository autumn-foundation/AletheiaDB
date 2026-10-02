{
  "lastUpdate": 1790949577256,
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
          "id": "fdb54a93706d9225622e7bba659f28474a82c7dc",
          "message": "fix(docker): smoke test's AS OF read truncates \"now\" to before the node exists (#3855)\n\nThe Docker Image workflow has failed on every trunk push since at least\n2026-09-22, at step 3 of `scripts/docker-smoke.sh`:\n\n```\ncreate: {\"success\":true,\"data\":{\"id\":0,\"label\":\"Person\",\"properties\":{\"name\":\"Alice\"}}}\nOK: created node 0\nOK: queried node back\nas-of: {\"success\":true,\"data\":[]}\nFAIL: AS OF query did not return Alice\n```\n\nBecause the smoke job gates publishing, no `:trunk` or `:sha-*` image\nhas been pushed since then.\n\n## Cause\nThe script builds the AS OF coordinate as `$(( $(date +%s) * 1000000\n))`. `date +%s` has whole-second resolution, and the create and the AS\nOF read run about 30 ms apart, so they almost always fall in the same\nsecond. The coordinate then truncates to a moment *before* the node was\ncreated, and the empty result is correct. The server is behaving as\ndesigned; the bug is in the test.\n\n## Fix\n`sleep 1` before taking the timestamp, so the truncated value is\nstrictly after the create. `date +%s%6N` would avoid the wait, but it's\nGNU-only and this script also runs on macOS.\n\nThis PR also carries the Rust 1.99 `fetch_update` lint fix from #3853\n(`6a962044`). Without it, Linting fails on every branch cut from trunk.\n\n## Test plan\n- [x] Reproduced against a local `aletheia-server` (`--features\nhttp-server,mcp-server`, same calls as the script):\n  - whole-second AS OF → `[]`\n  - microsecond AS OF → Alice\n  - whole-second AS OF taken a second later → Alice\n- [x] `bash -n scripts/docker-smoke.sh`\n- [x] The Docker Image workflow on this PR passes (the script is in its\npath filter, so it runs here)\n\nAlso ported into #3854, which touches `docker.yml` and so runs the same\njob.\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n\nhttps://claude.ai/code/session_01GE9sEPXcFxTuGcNhGw3uiU\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-10-02T08:49:15-05:00",
          "tree_id": "340f7302edc29f542bce63e59f7f47fe653e9d46",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/fdb54a93706d9225622e7bba659f28474a82c7dc"
        },
        "date": 1790949577255,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 23.042966753990505,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 206.4218474781498,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 242.95308856755028,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 242.5414010710499,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 197.94421111624956,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 412113.4661281685,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}