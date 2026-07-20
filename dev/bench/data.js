{
  "lastUpdate": 1784513432721,
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
          "id": "771bbb962cb7daaf48dba5e93e0083b5928d8180",
          "message": "Knowledge half-life analytics (#3377) (#3734)\n\nKnowledge half-life analytics for Issue #3377 — read-only survival\nanalysis over bi-temporal version history. **Implementation complete and\nreview-clean.**\n\n## Before / After\n\n- **Before:** AletheiaDB records every fact's full supersession history\nacross valid and transaction time, but nothing measures *how long facts\nof a given kind stay true*. Agents serve stale answers for volatile\nfacts and over-refresh stable ones, with no principled \"how current is\nthis knowledge base?\" answer.\n- **After:** read-only survival analysis over that history yields\nper-cohort **volatility statistics** (Kaplan–Meier half-life +\ndispersion + counts), per-fact **freshness scores** (age in half-lives +\nsurvival probability), and **staleness inventories** (the operator's\nre-verification worklist). Read-only: no writes, no storage-format\nchange, no write-path hook. Gated behind the experimental\n`semantic-temporal` cohort (ADR-0050).\n\n## How\n\nSurvival analysis over bi-temporal version history: each fact version's\nvalid-time lifespan is one observation. **Kaplan–Meier**\n(non-parametric, censoring-aware) gives the half-life = KM median. The\nterminating-event vs censoring decision **reuses the #3362\n`RevisionClass` oracle** (WorldChange/Retraction terminate a lifespan;\nCorrection/Reaffirmation continue it; an open valid interval =\nright-censored). Cohort scans are bounded by the\n`max_schema_as_of_entities` cap with a `sampled` flag; as-of\ntransaction-time makes reports replayable/pinnable (#3370). The pure\nestimator is separated from the scan and the freshness/staleness\nsurfaces so the statistics contract is unit-testable without a database.\nAn in-memory, recomputable `CohortStatsCache` (off the write path,\nWAL-generation-invalidated for \"now\" keys) backs the sub-millisecond\nwarm freshness path.\n\n## AC / Success-Metric evidence\n\nLine numbers are on `src/experimental/temporal/half_life.rs` unless\nnoted. Tests: `--lib half_life` (12) + `tests/half_life_e2e.rs` (20),\nall green under `--features semantic-temporal,simulation`.\n\n| AC / SM | Status | Evidence (symbol · file:line — test) |\n|---|---|---|\n| **AC1** per label/edge-type/property cohort; median half-life + count\n+ dispersion; censoring-aware; fixture recovers planted lifetime |\n**Met** | `enum Cohort` (:186, 3 granularities), `VolatilityStats`\n(:313), `kaplan_meier` (:514), `summarize` (:563) —\n`km_recovers_planted_half_life_within_tolerance`,\n`knowledge_half_life_recovers_planted_median_over_db_path`,\n`node_label_and_edge_type_cohorts_both_work`,\n`property_cohort_classifies_per_property` |\n| **AC2** retractions/deletions = end-of-life; correction-of-record vs\nchange-in-world distinguished; \"superseded\" explicit + reproducible |\n**Met** | `RevisionClass::classify` `pub(crate)`\n(`belief_revision.rs:401`); deterministic pure estimator —\n`retraction_is_end_of_life`, `correction_does_not_count_as_end_of_life`,\n`multiple_corrections_between_world_changes_are_one_lifespan`,\n`property_cohort_unrelated_change_does_not_terminate`,\n`retraction_lifespan_duration_equals_valid_interval`,\n`as_of_report_is_reproducible_across_wall_clock_advance` |\n| **AC3** insufficient observations → `insufficient_data`, never\nspurious | **Met** | `MIN_EVENTS_FOR_ESTIMATE = 20` (:163), `summarize`\n(:563), `VolatilityStats.insufficient_data` —\n`below_floor_is_insufficient_data`, `empty_cohort_is_insufficient_data`\n|\n| **AC4** freshness on-demand (per-entity call) + opt-in MCP annotation,\nnever silently altering responses | **Partially met** | Rust per-entity\ncall: `FreshnessScore` (:346), `fact_freshness` (:920),\n`survival_probability` (:659) —\n`fact_freshness_reports_age_in_half_lives`,\n`freshness_survival_probability_reads_curve`,\n`back_to_back_freshness_calls_return_identical_results`. **Deferred:**\nthe opt-in MCP-read annotation (MCP tools designed-not-registered — see\nflags) |\n| **AC5** staleness inventory, absolute/half-life threshold, paginated\nper MCP list conventions | **Partially met** | Rust surface complete:\n`StalenessThreshold::{AbsoluteAge,HalfLives}` (:365), `StalenessPage`\n(:390, offset/limit + `next_offset`), `staleness_inventory` (:992) —\n`staleness_inventory_filters_paginates_and_flags_sampled`,\n`staleness_half_lives_threshold_exact_count`. **Deferred:** MCP tool\nregistration |\n| **AC6** as-of past transaction time; replayable/pinnable (#3370) |\n**Met** | `HalfLifeOptions::with_as_of_transaction_time` (:250) +\n`as_of_transaction_time` field —\n`as_of_transaction_time_excludes_later_revisions`,\n`as_of_pinned_cache_is_stable_across_later_writes`,\n`as_of_none_cache_recomputes_after_new_committed_write` |\n| **AC7** bounded/capped scan, truncation flagged, no write-path impact\n| **Met** | `HalfLifeOptions::with_max_entities` (:264), `sampled` flag,\n`CohortStatsCache` off write path (:724) — `scan_cap_sets_sampled_flag`,\n`cache_reuses_and_invalidates_on_write_generation` |\n| **AC8** coverage caveats explicit (hot + restored; cold-migrated per\n#3238); response states window | **Met** | `CoverageScope::hot_only()`\n(:284, `hot_tier=true`, `cold_migrated_included=false`) surfaced on\nevery result — `summarize_discloses_hot_only_coverage`,\n`coverage_scope_is_disclosed_on_results` |\n| **AC9** experimental feature flag (ADR-0050) + graduation checklist |\n**Met** | `#[cfg(feature = \"semantic-temporal\")]` gate throughout;\ngraduation checklist in\n`docs/plans/2026-07-19-knowledge-half-life-analytics.md` |\n| **SM1** estimator ±10% for ≥100 obs incl 30% censored | **Met** |\nVerified by `km_recovers_planted_half_life_within_tolerance` (lib,\nplanted lifetimes with censoring) +\n`knowledge_half_life_recovers_planted_median_over_db_path` (e2e) |\n| **SM2** ≥30% stale-answer reduction in a #3366-harness scenario |\n**Deferred** | The #3366 eval harness is not yet integrated; this PR\nproduces the survival/recency signal, the harness scenario is a\nfollow-up |\n| **SM3** cohort 1M versions < 10s (batch); freshness < 1ms warm\n(cached) | **Bench-reported (not CI-gated)** | `benches/half_life.rs`:\n`fact_freshness_warm` = **799.56 ns** (« 1 ms target — Met).\n`knowledge_half_life_cohort_1500x4_cold` = **1.907 ms** for 6,000\nversions (proxy). **O(k²) caveat:** the KM step-merge is\nper-distinct-event-time, so extrapolating 6k→1M is unreliable (linear ≈\n0.32 s, worst-case quadratic ≈ 50 s); a true 1M-version run is deferred,\nhence bench-reported rather than a hard CI gate |\n\n## Review & fixes\n\nA 4-lens review (correctness / API / statistics / write-path-isolation)\nfound **3 correctness bugs + a validation gap + cache dead-code**, all\nfixed:\n\n- `6b0c41b` — Fix-A: half-life correctness/validation defects.\n- `9ec5165` — Fix-A tests + design-doc refresh.\n- `83c8fbf` — Fix-B: wire the cohort-stats cache + config cap +\nbenchmark.\n\nAll 32 gated tests pass; `cargo build --features semantic-temporal`,\nboth project clippy feature-sets\n(`semantic-temporal,mcp-server,simulation` and\n`config-toml,mcp-server,sharding-rpc,simulation`) with `-D warnings`,\nand `cargo fmt --all --check` are clean on the merged tree.\n\n## Coordinator flags\n\n- **`RevisionClass::classify` promoted to `pub(crate)`**\n(`belief_revision.rs:401`) so the estimator can reuse the #3362\nterminating-event oracle rather than duplicating the precedence rule.\n`RevisionClass` itself was already `pub`.\n- **MCP tools designed, not registered** — the `knowledge_half_life` /\n`fact_freshness` / `staleness_inventory` MCP surfaces are specified in\nthe design doc but not wired into the tool registry in this PR (the\ncoordinator batches MCP registration); the Rust API is complete. This is\nthe source of the AC4/AC5 \"Partially met\".\n- **`semantic-temporal` is absent from the CI clippy feature set** — the\ngated module is not linted by main CI; lint it manually with the two\nfeature-sets above.\n- **In-memory cache, no durable format** — half-life stats are\nrecomputable; no sidecar, no on-disk format change.\n- **#3238 cold-tier caveat** applies to coverage: v1 scans hot-tier\nheads only (`CoverageScope { cold_migrated_included: false }`),\ndisclosed on every response.\n\nCloses #3377.\n\n---------\n\nCo-authored-by: Claude <noreply@anthropic.com>",
          "timestamp": "2026-07-19T20:56:23-05:00",
          "tree_id": "5447c05333bc8149d0df94835b58abb85226352b",
          "url": "https://github.com/autumn-foundation/AletheiaDB/commit/771bbb962cb7daaf48dba5e93e0083b5928d8180"
        },
        "date": 1784513432720,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "target_3_hop/traverse_three_hops",
            "value": 159.75755136978034,
            "unit": "ns"
          },
          {
            "name": "target_single_hop/traverse_one_hop",
            "value": 20.22825165248418,
            "unit": "ns"
          },
          {
            "name": "target_batch_insertion/insert_1000_edges",
            "value": 498401.24174314935,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/worst_case_9_deltas",
            "value": 194.15758693853317,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/at_anchor",
            "value": 184.0570502496978,
            "unit": "ns"
          },
          {
            "name": "target_time_travel/with_5_deltas",
            "value": 182.49858795311374,
            "unit": "ns"
          }
        ]
      }
    ]
  }
}