# AletheiaDB → WebAssembly: Feasibility Assessment & Roadmap

- **Status:** Feasibility spike (design-only; no engine code in this PR)
- **Date:** 2026-07-20
- **Scope:** Empirical breakage inventory + realistic v1 shape + phased roadmap. NOT a port.
- **Verdict:** **Feasible-with-significant-work / not-yet.** An ephemeral in-memory profile is architecturally reachable, but it is blocked *below the source line* by non-optional native dependencies. This is dependency-graph surgery, not a spike of a few dozen gated call sites.

---

## 1. Executive summary

We attempted the honest first question — *does `--no-default-features` compile for wasm today, and if not, how far off is it?* — and measured it on two targets. **Neither target compiles a single line of AletheiaDB's own source.** Both builds die in the dependency layer:

- `wasm32-unknown-unknown --no-default-features` → dies at **`getrandom 0.2.17`** (hard `compile_error!`: the wasm-unknown target needs the `js` feature). Reached via the always-compiled crypto path (`aes-gcm`/`chacha20poly1305` → `rand_core` → `getrandom`).
- `wasm32-wasip1 --no-default-features` → clears getrandom (WASI has an RNG backend) and dies one layer deeper at the **native C/C++ FFI crates**: `cxx` (← `usearch`, the C++ HNSW library) with `fatal error: 'algorithm' file not found`, and `zstd-sys` (← `zstd`) with `fatal error: 'bits/libc-header-start.h' file not found`. No WASI C/C++ sysroot is configured, and `usearch` is C++ that is not realistically wasm-targetable regardless.

Because the compiler never reaches AletheiaDB source, the source-level wasm gaps (`std::fs`, `std::thread`, `SystemTime::now`, `Instant::now`, `mmap`) are currently **invisible** — they cannot be counted until five non-optional dependencies are made optional and stubbed/replaced. That dependency refactor is the real Phase 1, and it is the gate on everything else.

## 2. Why WASM — the motivation

1. **Browser-side agent memory (the killer use).** The wheelhorse CRM is an autumn app that is Tauri-capable. An in-client AletheiaDB — running in the renderer or a web worker — gives an LLM agent a *local, offline, private* bi-temporal memory: it can record what it learned, when it learned it, and reason "as of" a past point, with no server round-trip and no data leaving the device. Bi-temporality is exactly the shape of agent memory (what was true, and when we came to believe it).
2. **Edge runtimes.** Cloudflare Workers / Fastly / Deno Deploy run wasm with tight CPU/memory budgets and no local filesystem. An in-memory Aletheia is a natural fit for per-request or per-session graph reasoning at the edge.
3. **In-browser docs playground.** A wasm build powers a live "try AQL/Cypher in the page" playground — the single highest-leverage doc asset for a query language, and a zero-backend demo.

## 3. Empirical breakage inventory (Step 1)

Targets installed: `wasm32-unknown-unknown`, `wasm32-wasip1` (rustc 1.97). Command: `cargo check --target <T> --no-default-features`. Full logs are attached as Appendix B.

### 3.1 Where each target dies

| Target | Reached AletheiaDB source? | First fatal blocker | Root cause |
|---|---|---|---|
| `wasm32-unknown-unknown` | **No** (0 source errors emitted) | `getrandom 0.2.17` `compile_error!` | wasm-unknown needs `getrandom` `js` feature; pulled via crypto path |
| `wasm32-wasip1` | **No** (0 source errors emitted) | `cxx` (←usearch) + `zstd-sys` (←zstd) | C/C++ FFI, no WASI sysroot; usearch is C++ |

The key finding is the *layer* of failure: **dependency, not source.** "How far from compiling" is not measurable in source-error counts yet — it is measurable in dependency blockers, of which there are five fundamental ones.

### 3.2 Ranked non-optional dependency blockers

All are non-optional in `Cargo.toml` (default features are `config-toml`, `audit-export`, `simsimd`; these five are outside default and cannot be dropped by `--no-default-features`).

| # | Dep | wasm viability | Why it blocks | Mitigation |
|---|---|---|---|---|
| 1 | `usearch` 2.25.2 | **None** (C++ via `cxx`) | C++ HNSW lib; no WASI C++ sysroot; not wasm-targetable | Gate vector index out of the wasm profile, or swap to a pure-Rust k-NN (e.g. `instant-distance`/`hnsw_rs`) behind a feature |
| 2 | `zstd` 0.13 | **None** as bundled C (`zstd-sys`) | Bundled C; needs WASI sysroot | Swap to pure-Rust `ruzstd` (decode) / gate compression out; anchor+delta history can run uncompressed in-memory |
| 3 | crypto path → `getrandom 0.2` | Needs `js`/wasi backend | Only Build-A blocker; `compile_error!` on wasm-unknown | Add target-gated `getrandom = { version = "0.2", features = ["js"] }`; longer term move to `getrandom 0.3` |
| 4 | `memmap2` 0.9 | **None** | `mmap` syscall absent on both wasm targets | Only used by WAL segment reader + index persistence graph load; both compiled out of the wasm profile |
| 5 | `redb` 4.1 | **None** | Needs mmap + real filesystem | Cold tier is optional-by-design; gate the redb backend out of the wasm profile |
| 6 | `rayon` 1.12 | None (needs OS threads) | Parallel iterators need threads | Feature-gate to serial iterators on wasm (`cfg`-select `into_iter` vs `into_par_iter`) |
| 7 | `crc32fast` 1.4 | Likely OK (pure-Rust fallback) | Not observed to fail | None needed |
| 8 | `libc` 0.2 | Partial (stubs) | Many symbols stubbed on wasm | Audit the few direct uses; most are fs/thread-adjacent and gated out with their callers |

`simsimd` is already `optional` and was off here, but it is another C dep on the *default* path — the wasm profile must keep it off and rely on the scalar distance fallback.

### 3.3 Source-level surface (estimate, not yet compiler-confirmed)

Once the dependency layer is cleared, the source-level gates become visible. Raw `grep` line-hit counts across `src/` (includes tests and feature-gated modules that `--no-default-features` already excludes, so these are **upper bounds**, not the wasm-profile figure):

| Category | Raw hits | Concentrated in |
|---|---|---|
| `std::fs` | 413 | WAL, index-persistence, cold-storage, encryption, backup, rotation |
| `std::thread` | 233 | WAL concurrent system, interner, group-commit, experimental temporal/reasoning |
| `sync_all`/`fsync` | 191 | WAL, rotation, provenance chain, transaction write path |
| `Instant::now` | 129 | migration, sharding, config, write path, group-commit |
| `SystemTime::now` | 41 | index-persistence, checkpoint, migration |
| `memmap` | 5 | WAL segment reader, index-persistence graph |

The always-compiled surface under `--no-default-features` is `api`, `config`, `core`, `db`, `encryption`, `index`, `provenance_chain`, `query`, `storage`, `experimental`, `prelude`. The bulk of the fs/thread/fsync hits live in `storage` (WAL, index-persistence, cold, backup) and `encryption` — i.e. the durability core — which is precisely what the ephemeral profile must compile *out*, so the effective wasm-profile site count is far below the raw totals but still well beyond "a few dozen."

## 4. Planning

### 4.1 Brainstorming — the idea space

- Compile the whole crate to wasm as-is. (Rejected: impossible — five native deps.)
- A `wasm-compat` feature that swaps every hostile dependency and `cfg`-gates every fs/thread/time site.
- Extract a dependency-light `aletheiadb-core` crate (graph + temporal + query, no storage/WAL/vector) that both native and wasm consume.
- A separate `aletheiadb-wasm` facade crate that depends on the core with a stripped in-memory config and owns the wasm-bindgen surface + `cdylib`.
- Persistence via OPFS/IndexedDB (browser) or WASI `fs` (server-side wasm) — post-v1.
- Vector search: drop it on wasm v1, or swap `usearch` → pure-Rust HNSW.
- Compression: run history uncompressed in-memory on wasm, or swap `zstd` → `ruzstd`.
- HLC clock: source "now" from `js-sys`/`web-sys` `performance.now()`/`Date.now()` instead of `SystemTime`/`Instant`.
- Concurrency: single-threaded engine; parallelism via multiple web workers each owning a shard, message-passing at the edge.

### 4.2 Reverse brainstorming — how would we *guarantee* this fails?

- Try to keep durability semantics (WAL + fsync) in a place with no fsync → silently lie about ACID. **Fix:** make the wasm profile explicitly *ephemeral*, and say so loudly in the type/name (`AletheiaDB::new()` only; no `open()`).
- Bury the dependency surgery inside "just add a feature flag" and discover mid-stream that `redb`/`memmap2`/`usearch` are threaded through non-optional module code. **Fix:** do the dependency-optional refactor first, as its own reviewable stage, before any wasm cfg work.
- Let the string interner grow unbounded in a 4 GB-capped browser tab → OOM the whole page. **Fix:** a hard interner cap + eviction is a v1 requirement, not a follow-up.
- Assume `Instant::now()`/`SystemTime::now()` "just work" and ship an engine whose HLC goes backwards or panics on wasm. **Fix:** a single `clock` abstraction with a wasm backend, unit-tested for monotonicity.
- Ship a `cdylib` that pulls in a transitively-native crate and fails at link, not check. **Fix:** gate at the *dependency* level (Cargo `cfg(target_arch)` tables), verified by `cargo check --target wasm32-unknown-unknown`, not just `cargo build` of the lib.

### 4.3 Six Thinking Hats

- **White (facts):** 0 source errors on both targets; 5 non-optional native blockers; getrandom is a one-line target-gated fix; edition 2024 / rustc 1.97; no `cdylib` crate-type today.
- **Red (gut):** the *ephemeral browser-memory* use is genuinely compelling and differentiated; the full-durability wasm port is not worth chasing.
- **Black (risks):** dependency surgery touches the storage hot path; interner/memory bounds in a tab; ACID semantics become misleading if not explicitly ephemeral; usearch removal loses vector search on wasm v1; maintaining a second dependency profile is ongoing tax.
- **Yellow (benefits):** offline local agent memory for the CRM; zero-backend docs playground; edge reasoning; a cleaner `core` extraction benefits native builds too (faster compile, clearer layering).
- **Green (creative):** web-worker-per-shard to reclaim parallelism; OPFS-backed WAL to recover *some* durability later; ship the playground first as the forcing function that keeps the wasm profile honest.
- **Blue (process):** stage it — (0) getrandom, (1) deps-optional, (2) source cfg + clock, (3) cdylib + bindings + smoke, (4) persistence. Each stage independently green on native CI *and* advancing the wasm frontier.

## 5. Implementation approaches (with tradeoffs)

### Approach A — `wasm-compat` feature inside the main crate
Add a feature that (a) target-gates dependencies via `[target.'cfg(...)'.dependencies]`, (b) `cfg`-gates every fs/thread/time site, (c) adds `cdylib`.
- **Pros:** one crate; no API split; incremental.
- **Cons:** pervasive `cfg` noise across the storage core; easy to regress; the main crate carries a large second build profile forever; `cdylib` on the main crate pulls the whole surface.

### Approach B — extract `aletheiadb-core` (recommended target architecture)
Pull the dependency-light engine (graph model, bi-temporal versioning, interner, query/AQL/Cypher over in-memory storage) into a `crates/aletheiadb-core` with **zero** storage/WAL/vector/compression deps. Native `aletheiadb` and a new `aletheiadb-wasm` both depend on it.
- **Pros:** the wasm boundary becomes a *dependency* boundary, not a `cfg` maze; core stays honest (can't accidentally `use std::fs`); benefits native compile times and layering; the wasm facade owns `cdylib` + bindings in isolation.
- **Cons:** the largest up-front refactor; must draw the seam between "engine" and "durability/index" cleanly; risk of churn in a mature codebase.

### Approach C — thin `aletheiadb-wasm` facade over the main crate with a stripped config
A new crate depending on `aletheiadb` with default-features off and a wasm feature, exposing wasm-bindgen. Still requires Approach A's dependency+cfg work underneath.
- **Pros:** isolates the bindings/`cdylib`; smallest *new* surface.
- **Cons:** does not by itself solve the dependency blockers — it is A plus a facade; the facade can't hide a non-optional native dep.

### Chosen: **A now, converging on B.**
Do the minimum dependency-optional work (Approach A mechanics: `cfg`-gated Cargo deps + a `wasm` feature) to get `cargo check --target wasm32-unknown-unknown` green for the graph+temporal+query surface, *while drawing the module seam so that the gated-in-memory engine is exactly the future `aletheiadb-core`*. This gets an early, compile-verified wasm frontier without a big-bang crate split, and leaves Approach B as a mechanical extraction once the seam is proven. Approach C's facade is adopted only at the `cdylib`/bindings stage (Phase 3), where isolating the wasm-bindgen surface in its own crate is genuinely cleaner.

## 6. The realistic v1 shape

- **Ephemeral, in-memory only.** `AletheiaDB::new()` (tempdir/in-RAM) — persistence, WAL, index-persistence, cold storage compiled out. **No `open()` on wasm.** Durability is explicitly *not* offered in v1; the type surface says so.
- **Single-threaded execution**, intended to run in a dedicated web worker so it never blocks the UI thread. `rayon` parallel iterators `cfg`-selected to serial on wasm.
- **Clock:** an internal `clock::now()` abstraction; wasm backend reads `js-sys`/`web-sys` (`Date.now()` for wallclock, `performance.now()` for monotonic), feeding the HLC. Native backend unchanged.
- **Vector search:** off in wasm v1 (usearch removed). Graph + bi-temporal + AQL/Cypher only. (Pure-Rust HNSW is a Phase-4 option.)
- **Compression:** history held uncompressed in RAM (anchor+delta logic unchanged; only the zstd codec is gated out).
- **RNG:** `getrandom` `js` backend for the crypto/audit hashing that survives.
- **Crate output:** a `cdylib` (added; today the crate is rlib-only) in an `aletheiadb-wasm` facade, with a minimal `wasm-bindgen` smoke surface: create node → update node → as-of read. Compile-verified only (`cargo check --target wasm32-unknown-unknown`); no browser required for the spike.

## 7. Risks & edge cases as test cases

Each risk is written as the test that would catch its regression.

1. **Native build unaffected by wasm gating.** `cargo check --no-default-features --tests` and the full CI feature set stay green after every stage. (Guard against `cfg` mistakes leaking into native.)
2. **wasm frontier actually advances.** A CI job runs `cargo check --target wasm32-unknown-unknown -p aletheiadb-wasm` and must pass once Phase 3 lands; before then, a job asserts the build gets *past* the dependency layer (no getrandom/cxx/zstd-sys failure).
3. **HLC monotonicity on wasm.** Unit test: 10k successive `clock::now()` calls are non-decreasing and the HLC logical counter increments on same-wallclock ties — with the wasm clock backend injected.
4. **Ephemeral honesty.** Compile-fail (trybuild) test: `AletheiaDB::open(path)` does not exist in the wasm profile; only `new()` does. A doc test asserts data does not survive a dropped instance.
5. **Interner memory bound.** Test: inserting N distinct labels past the cap triggers eviction/refusal, not unbounded growth (the browser-tab OOM guard). Ties to the existing interner-cap tests.
6. **Vector API absence is explicit, not a panic.** On wasm, `vector_index(...)`/`find_similar(...)` are either `cfg`-compiled-out (compile error at call site) or return a structured "unavailable on this platform" error — never a runtime panic. Test both the compile-out and the error-return variant chosen.
7. **Serial-vs-parallel equivalence.** A property test: results of an operation that uses `rayon` natively are identical when run through the wasm serial path (same inputs → same output set/order guarantees).
8. **zstd removal is lossless for in-memory history.** Round-trip test: write history, reconstruct an old version, assert byte-identical to native — with compression gated off.
9. **getrandom js gate does not regress native.** Native still uses the default backend; the `[target.'cfg(target_arch = "wasm32")']` table never affects x86_64.
10. **as-of read correctness on wasm.** The smoke example (create → update → as-of read of the pre-update value) is an integration test compiled for both native and wasm; native runs it, wasm compile-checks it.

## 8. Phased roadmap

Effort tiers are relative scope, not calendar estimates.

- **Phase 0 — getrandom (XS).** Add target-gated `getrandom` `js` feature. Clears Build-A's first blocker. Zero native impact. One-line Cargo change; verifiable immediately.
- **Phase 1 — dependency-optional surgery (L, the gate).** Make `usearch`, `zstd`, `memmap2`, `redb`, `rayon` optional; introduce a `wasm`/`wasm-compat` feature that turns them off and selects in-memory/serial alternatives. Move each behind a backend abstraction where a use is non-trivial (vector, compression codec, cold tier, parallel iterators). This is where the real work is; it is invisible until done because the build can't reach source before it. **Deliverable:** `cargo check --target wasm32-unknown-unknown --features wasm-compat --no-default-features` reaches AletheiaDB source and emits its *first* source errors — at which point the true source-site count becomes measurable.
- **Phase 2 — source cfg + clock (M).** Gate the surviving `std::fs`/`std::thread`/`SystemTime`/`Instant`/`mmap` sites; introduce the `clock` abstraction with a wasm backend; drive `cargo check --target wasm32-unknown-unknown` to green for the graph+temporal+query surface.
- **Phase 3 — cdylib + bindings + smoke (M).** Add the `aletheiadb-wasm` facade crate with `crate-type = ["cdylib"]`, a minimal `wasm-bindgen` API (create/update/as-of read), and a compile-verified smoke example. Add the wasm CI job.
- **Phase 4 — persistence & scale (L, post-v1).** OPFS/IndexedDB-backed persistence for the browser (recovering *some* durability); WASI `fs` for server-side wasm runtimes; optional pure-Rust HNSW to restore vector search; web-worker-per-shard for parallelism.

## 9. Honest hard parts

- **Bi-temporal durability without fsync.** On wasm v1 there is no fsync and no WAL. "Transaction time" still orders writes within a session, but *durability* is gone — a tab close loses everything. The semantics we keep are *temporal reasoning*, not *crash recovery*. This must be stated in the API, not implied. OPFS (Phase 4) recovers a weaker durability (async, no true fsync barrier) — enough for agent memory, not for a system of record.
- **Interner memory bounds in a tab.** The string interner is unbounded by default; in a 4 GB-capped tab that is an OOM vector. A hard cap + eviction is a v1 requirement (there are already interner-cap tests to build on).
- **What temporal guarantees mean when ephemeral.** "As of 2024-01-01" is meaningful only over data recorded *this session*. The value proposition is an agent's within-session (or OPFS-persisted) memory, not a durable historical archive. Positioning must be precise to avoid overpromising ACID.
- **Second build profile is ongoing tax.** Every future storage-touching change must keep the wasm profile green. The Approach-B core extraction is what makes this sustainable (the core physically cannot regress into `std::fs`), which is why A converges to B.

## Appendix A — full dependency blocker table
See §3.2. Non-optional in `Cargo.toml`: `usearch`, `zstd`, `memmap2`, `redb`, `rayon`, `crc32fast`, `libc`. Optional but on-by-default and native: `simsimd`.

## Appendix B — raw build-failure excerpts

`wasm32-unknown-unknown --no-default-features` (first fatal):
```
error: the wasm*-unknown-unknown targets are not supported by default, you may need to enable the "js" feature. For more information see: https://docs.rs/getrandom/#webassembly-support
   --> getrandom-0.2.17/src/lib.rs:346:9
    = note: this comes via aes-gcm/chacha20poly1305 -> rand_core -> crypto-common -> getrandom
```

`wasm32-wasip1 --no-default-features` (first fatal, two native crates):
```
error: failed to build archive / cc invocation
  cxx v1.0.198: clang++ --target=wasm32-wasip1 ... cxx.cc: fatal error: 'algorithm' file not found   (cxx <- usearch)
  zstd-sys v2.0.16: clang --target=wasm32-wasip1 ... zstd_v07.c: fatal error: 'bits/libc-header-start.h' file not found   (no WASI sysroot; zstd-sys <- zstd)
```

## Appendix C — always-compiled module surface under `--no-default-features`
`api`, `config`, `core`, `db`, `encryption`, `index`, `provenance_chain`, `query`, `storage`, `experimental`, `prelude`. The fs/thread/fsync concentration is in `storage` (WAL, index-persistence, cold, backup, rotation) and `encryption` — the durability core the ephemeral profile compiles out.
