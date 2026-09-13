{
  "lastUpdate": 1789259769847,
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
          "id": "3d85e75420b3455cb4ad05132f4c26b09ae3d3d0",
          "message": "chore(sec): resolve RUSTSEC-2026-0235, narrow RUSTSEC-2026-0258 ignor… (#3825)\n\n…e (fixes #3807)",
          "timestamp": "2026-09-12T19:22:27-05:00",
          "tree_id": "21ecc108081afc15733f5bc74ef16f72ccdb322c",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/3d85e75420b3455cb4ad05132f4c26b09ae3d3d0"
        },
        "date": 1789259769847,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 22.11632282121448,
            "unit": "ns"
          },
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 183.77967690291374,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 192.14165732122675,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 200.09145809313907,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 192.37982613531048,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 390238.89371028275,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}