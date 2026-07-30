{
  "lastUpdate": 1785438730128,
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
          "id": "ed94af010c0a69974e226fe3958d3fab729bf9dd",
          "message": "ci: grant contents:write permissions to release workflow (#3790)\n\nFixes 403 'Resource not accessible by integration' error during\ncreate-release step by explicitly adding contents:write permissions.",
          "timestamp": "2026-07-30T13:58:46-05:00",
          "tree_id": "1e081f915bbaa59d1348b3fbf2650cf9245bb1c8",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/ed94af010c0a69974e226fe3958d3fab729bf9dd"
        },
        "date": 1785438730128,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 489860.83647558326,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 172.67905089056305,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 189.40539376779566,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 187.49498895641372,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 195.85372206936503,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 21.68305634984586,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}