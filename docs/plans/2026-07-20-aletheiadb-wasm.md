# AletheiaDB → WebAssembly: Feasibility Assessment & Roadmap

- **Status:** Feasibility spike (design-only; no engine code in this PR)
- **Date:** 2026-07-20
- **Scope:** Empirical breakage inventory + realistic v1 shape + phased roadmap. NOT a port.
- **Verdict:** **Feasible-with-significant-work / not-yet.** An ephemeral in-memory profile is architecturally reachable, but it is blocked *below the source line* by non-optional native dependencies. This is dependency-graph surgery, not a spike of a few dozen gated call sites.

---

## 1. Executive summary

We attempted the honest first question — *does `--no-default-features` compile for wasm today, and if not, how far off is it?* — and measured it on two targets. **Neither target compiles a single line of AletheiaDB's own source.** Both builds die in the dependency layer:

- `wasm32-unknown-unknown --no-default-features` → dies at **`getrandom 0.2.17`** (hard `compile_error!`: the wasm-unknown target needs the `js` feature). `getrandom 0.2` is **over-determined** in this graph — it is forced by at least three independent non-optional paths: the AEAD crypto path (`aes-gcm`/`chacha20poly1305` → `aead` → `crypto-common` → `rand_core`), the KDF path (`argon2`/`pbkdf2` → `digest` → `crypto-common` → `rand_core`), **and the direct non-optional `rand = "0.8"` dependency** → `rand_core`. So gating out encryption alone would *not* remove it; the fix is the crate-level `js`/wasi backend (Phase 0), which works regardless of which path pulls it.
- `wasm32-wasip1 --no-default-features` → clears getrandom (WASI has an RNG backend) and dies one layer deeper at the **native C/C++ FFI crates**: `cxx` (← `usearch`, the C++ HNSW library) with `fatal error: 'algorithm' file not found`, and `zstd-sys` (← `zstd`) with `fatal error: 'bits/libc-header-start.h' file not found`. No WASI C/C++ sysroot is configured.

Because the compiler never reaches AletheiaDB source, the source-level wasm gaps (`std::fs`, `std::thread`, `SystemTime::now`, `Instant::now`, `mmap`) are currently **invisible** — they cannot be counted until the non-optional native dependencies are made optional and stubbed/replaced. That dependency refactor is the real Phase 1, and it is the gate on everything else.

## 2. Why WASM — the motivation

Ordered by how fully the *ephemeral in-memory* v1 shape actually enables each:

1. **In-browser docs playground (fully enabled by v1).** A wasm build powers a live "try AQL/Cypher in the page" playground — the single highest-leverage doc asset for a query language, a zero-backend demo, and (per the Green hat below) the honest forcing function that keeps the wasm profile from rotting. This use needs nothing beyond the ephemeral graph+temporal+query core.
2. **Browser-side agent memory (the strategic prize; needs the vector swap + OPFS).** The wheelhorse CRM is an autumn app that is Tauri-capable. An in-client AletheiaDB — running in a web worker — gives an LLM agent a *local, private* bi-temporal memory: what it learned, when it learned it, and "as of" reasoning over a past point, with no server round-trip. Two honest caveats the v1 shape imposes: (a) agent recall is overwhelmingly *semantic* (embedding similarity), so this use needs vector search, which means the pure-Rust HNSW swap (see §3.2, §6) — it is *not* served by graph/property lookup alone; (b) until Phase 4 OPFS persistence lands, the memory is a **within-session working set** that a tab close discards, not a durable store. v1 serves single-session agent memory; durable agent memory is Phase 4.
3. **Edge runtimes (needs a hydrate path).** Cloudflare Workers / Fastly / Deno Deploy run wasm with tight CPU/memory budgets and no local filesystem. An in-memory Aletheia fits per-request or per-session graph reasoning at the edge — but a stateless edge invocation needs a cheap way to *hydrate* state per request (from KV/D1/R2), which an ephemeral-only build does not itself provide; that hydrate path is a Phase 4 concern.

Bi-temporality is exactly the shape of agent memory (what was true, and when we came to believe it), which is what makes uses 1–2 differentiated rather than generic.

## 3. Empirical breakage inventory (Step 1)

Targets installed: `wasm32-unknown-unknown`, `wasm32-wasip1`. Toolchain used: `rustc 1.97.0` (repo MSRV is `rust-version = 1.92`). Command: `cargo check --target <T> --no-default-features`. Full logs are attached as Appendix B.

### 3.1 Where each target dies

| Target | Reached AletheiaDB source? | First fatal blocker | Root cause |
|---|---|---|---|
| `wasm32-unknown-unknown` | **No** (0 source errors emitted) | `getrandom 0.2.17` `compile_error!` | wasm-unknown needs `getrandom` `js` feature; over-determined (crypto + KDF + direct `rand`) |
| `wasm32-wasip1` | **No** (0 source errors emitted) | `cxx` (←usearch) + `zstd-sys` (←zstd) | C/C++ FFI, no WASI sysroot |

The key finding is the *layer* of failure: **dependency, not source.** "How far from compiling" is not measurable in source-error counts yet — it is measurable in dependency blockers.

### 3.2 Non-optional dependency blockers

All are non-optional in `Cargo.toml` (default features are `config-toml`, `audit-export`, `simsimd`; the deps below are outside default and cannot be dropped by `--no-default-features`). This is more than "five fundamental" blockers — the native-FFI and thread/fs core is joined by a non-optional CLI/TUI stack.

| # | Dep | wasm viability | Why it blocks | Mitigation |
|---|---|---|---|---|
| 1 | `usearch` 2.25.2 | Very high effort (C++ via `cxx`) | C++ HNSW lib; requires a WASI C++ sysroot (wasi-sdk) not configured here | Swap to a pure-Rust k-NN (`instant-distance`/`hnsw_rs`) behind a backend feature — this is what restores the agent-memory use case (§6) |
| 2 | `zstd` 0.13 (`zstd-sys`) | **None** as bundled C | Bundled C; needs WASI sysroot | Swap the codec to a pure-Rust, **bidirectional** one (`lz4_flex`, or `miniz_oxide` deflate) — *not* `ruzstd` (decode-only). Keep compression ON: it is *more* valuable on the memory-capped target, not less (§9) |
| 3 | crypto/KDF/`rand` → `getrandom 0.2` | Needs `js`/wasi backend | Only Build-A blocker; over-determined (see §1) | Target-gated `getrandom = { version = "0.2", features = ["js"] }`. `aes-gcm`/`chacha20poly1305` are pure-Rust and compile to wasm, so encryption need **not** be gated out to build — only its fs-touching at-rest path |
| 4 | `memmap2` 0.9 | **None** | `mmap` syscall absent on both wasm targets; this is the *only* mmap consumer in the tree | Used by WAL segment reader + index-persistence graph load; both compiled out of the wasm profile |
| 5 | `redb` 4.1 | **None** | Needs a real filesystem (`std::fs::File` I/O). Note redb 4.x does **not** mmap — it dropped memory-mapping in its 2.0 rewrite | Cold tier is optional-by-design; gate the redb backend out of the wasm profile |
| 6 | `rayon` 1.12 | None (needs OS threads) | Parallel iterators need threads | `cfg`-select serial iterators on wasm. The `par_iter` sites concentrate in `index/vector/*`, which is already gated out of the ephemeral profile — so the serial-fallback surface is small |
| 7 | `crossterm` 0.29 + `comfy-table` 7.2 | **None** | Non-optional CLI/TUI: pull `mio` (OS epoll/kqueue/IOCP reactor) + `signal-hook`; `clap`/`clap_complete` also always-compiled | The CLI (`bin/aletheia`) is not part of the wasm library surface; the wasm profile must exclude the terminal/CLI stack (a further reason to isolate the wasm build in its own facade crate, §5) |
| 8 | `crc32fast` 1.4 / `libc` 0.2 | crc32fast OK (pure-Rust fallback); libc partial (stubs) | Low risk | Audit the few direct `libc` uses; most are fs/thread-adjacent and gated out with their callers |

`simsimd` is already `optional` and was off here, but it is another C dep on the *default* path — the wasm profile must keep it off and rely on the scalar distance fallback.

### 3.3 Source-level surface (estimate, not yet compiler-confirmed)

Once the dependency layer is cleared, the source-level gates become visible. Raw `grep` line-hit counts across `src/` (includes tests and feature-gated modules that `--no-default-features` already excludes, so these are **upper bounds**, not the wasm-profile figure). The "bigger lift" verdict rests primarily on the §3.2 dependency-surgery gate; these counts are corroborating but unquantified for the wasm profile:

| Category | Raw hits | Concentrated in |
|---|---|---|
| `std::fs` | 413 | WAL, index-persistence, cold-storage, encryption, backup, rotation |
| `std::thread` | 241 | WAL concurrent system, interner, group-commit, experimental temporal/reasoning |
| `sync_all`/`fsync` | 191 | WAL, rotation, provenance chain, transaction write path |
| `Instant::now` | 129 | migration, sharding, config, write path, group-commit (mostly modules gated out of the ephemeral profile) |
| `SystemTime::now` | 41 | index-persistence, checkpoint, migration |
| `memmap` | 5 | WAL segment reader, index-persistence graph |

A clock seam **already exists**: `time::now()` (`src/core/temporal.rs`) is the single HLC wallclock choke point, with a live injection hook used by the simulation `SimulatedClock`. The wasm clock backend is an *addition to that existing seam*, not net-new architecture (§8 Phase 2). Separately, the 129 scattered `Instant::now` monotonic/latency reads live largely in WAL/migration/sharding/group-commit — modules gated out of the ephemeral profile — so the HLC-clock work is nearly done and the `Instant` work is mostly out-of-profile.

The always-compiled surface under `--no-default-features` is `api`, `config`, `core`, `db`, `encryption`, `index`, `provenance_chain`, `query`, `storage`, `experimental`, `prelude`. The bulk of the fs/thread/fsync hits live in `storage` (WAL, index-persistence, cold, backup) and `encryption`'s at-rest path — precisely what the ephemeral profile compiles out — so the effective wasm-profile site count is far below the raw totals but still well beyond "a few dozen."

## 4. Planning

### 4.1 Brainstorming — the idea space

- Compile the whole crate to wasm as-is. (Rejected: impossible — the non-optional native deps.)
- A `wasm-compat` feature that swaps every hostile dependency and `cfg`-gates every fs/thread/time site.
- Extract a dependency-light `aletheiadb-core` crate that both native and wasm consume.
- A separate `aletheiadb-wasm` facade crate that owns the wasm-bindgen surface + `cdylib` and excludes the CLI/TUI stack.
- Persistence via OPFS/IndexedDB (browser) or WASI `fs` (server-side wasm) — post-v1.
- Vector search on wasm via a pure-Rust HNSW (`instant-distance`/`hnsw_rs`) behind the same backend abstraction used for the compression codec.
- Compression: swap the C `zstd` codec for a pure-Rust bidirectional codec — *keep it on*, because RAM is scarcer on wasm.
- HLC clock: source "now" from `js-sys`/`web-sys` (`Date.now()` wallclock, `performance.now()` monotonic) through the existing `time::now()` seam.
- Concurrency: single-threaded engine in one web worker; multi-worker sharding only with a distributed-clock protocol (see reverse-brainstorm).

### 4.2 Reverse brainstorming — how would we *guarantee* this fails?

- Keep WAL + fsync semantics where there is no fsync → silently lie about ACID. **Fix:** make the wasm profile explicitly *ephemeral* and say so in the type surface (`AletheiaDB::new()` only; no `open()`).
- Bury the dependency surgery inside "just add a feature flag" and discover mid-stream that `redb`/`memmap2`/`usearch`/`crossterm` are threaded through non-optional code. **Fix:** do the dependency-optional refactor first, as its own reviewable stage.
- Gate compression *off* to save build complexity, then OOM the tab because history accumulates uncompressed with no cold-tier escape valve. **Fix:** keep compression on via a pure-Rust codec; add a hot-version RAM-budget/eviction policy (§9).
- Let the string interner grow unbounded in a 4 GB-capped tab. **Fix:** the existing interner cap (`DEFAULT_MAX_INTERNED_STRINGS`) is necessary but **not sufficient** — it bounds string count, not version/history growth (§9).
- Propose "web-worker-per-shard" without a shared clock → each worker's independent HLC breaks the single monotonic transaction-time order bi-temporal correctness depends on. **Fix:** scope v1 to a *single* worker / single HLC; treat multi-worker sharding as needing a distributed-clock protocol, not a Phase-4 nicety (§9).
- Ship a `cdylib` that transitively pulls a native crate (e.g. `mio` via `crossterm`) and fails at link, not check. **Fix:** gate at the *dependency* level in a facade crate that never depends on the CLI stack; verify with `cargo check --target wasm32-unknown-unknown`.

### 4.3 Six Thinking Hats

- **White (facts):** 0 source errors on both targets; getrandom over-determined (crypto + KDF + direct `rand`); native-FFI (`usearch`/`zstd`) + non-optional CLI stack (`crossterm`→`mio`); a clock seam already exists (`time::now()`); `core/` is already dependency-clean (imports none of storage/index/encryption); no `cdylib` crate-type today.
- **Red (gut):** the *ephemeral browser-memory* and *docs-playground* uses are genuinely compelling; the full-durability wasm port is not worth chasing.
- **Black (risks):** dependency surgery touches the storage hot path; whole-history-uncompressed-in-RAM vs the wasm ~4GB ceiling; ACID semantics become misleading if not explicitly ephemeral; cross-worker HLC ordering is a *correctness* trap; a second build profile is ongoing maintenance tax.
- **Yellow (benefits):** offline/local agent memory for the CRM; zero-backend docs playground; edge reasoning; a `core` extraction (already clean) benefits native builds too (faster compile, clearer layering).
- **Green (creative):** ship the playground first as the forcing function; OPFS `createSyncAccessHandle` (worker-only) offers a synchronous `flush()`, so an OPFS-backed WAL can be stronger than a naive async store; pure-Rust HNSW restores semantic recall on wasm.
- **Blue (process):** stage it — (0) getrandom, (1) deps-optional + call-site cfg, (2) source cfg + clock, (3) cdylib + facade + bindings + smoke, (4) persistence + vector + scale. Each stage independently green on native CI *and* advancing the wasm frontier.

## 5. Implementation approaches (with tradeoffs)

### Approach A — `wasm-compat` feature inside the main crate
Add a feature that (a) target-gates dependencies via `[target.'cfg(...)'.dependencies]`, (b) `cfg`-gates every fs/thread/time site and each disabled dep's call sites, (c) adds `cdylib`.
- **Pros:** one crate; no API split; incremental.
- **Cons:** pervasive `cfg` noise across the storage core; easy to regress; the main crate carries a large second build profile forever; a `cdylib` on the main crate drags the whole surface, including the CLI/TUI stack it must not compile.

### Approach B — extract `aletheiadb-core` (target architecture)
Pull the dependency-light engine into a `crates/aletheiadb-core`. **Correction to the naive framing:** `core/` is *already* dependency-clean (it imports none of `storage`/`index`/`encryption`), so extracting the pure model is mechanical, not a big-bang split. The real work is that the wasm profile needs **query**, and `query → {storage, index}`: the in-memory `storage::current` (`CurrentStorage`) and the `index::adjacency` structures (minus the vector index) must move into — or behind a trait exposed by — the core crate. So `aletheiadb-core` is not "zero storage deps"; it is "graph model + bi-temporal versioning + interner + `storage::current` + `index::adjacency` + query, with the durability/vector/compression backends behind traits."
- **Pros:** the wasm boundary becomes a *dependency* boundary, not a `cfg` maze; core physically cannot regress into `std::fs`; benefits native compile times and layering; the facade owns `cdylib` + bindings in isolation.
- **Cons:** must draw the `query ↔ storage/index` seam cleanly (a storage trait); some churn in a mature codebase.

### Approach C — thin `aletheiadb-wasm` facade over the main crate
A new crate depending on `aletheiadb` (default-features off, wasm feature on), exposing wasm-bindgen and `cdylib`, and — critically — never depending on the CLI/TUI stack.
- **Pros:** isolates the bindings/`cdylib` and keeps `mio`/`crossterm` out of the wasm graph by construction.
- **Cons:** does not by itself solve the dependency blockers — it is A's dependency+cfg work plus a facade; the facade cannot hide a non-optional native dep inside the main crate.

### Chosen: **A now, converging on B, with C's facade adopted at Phase 3.**
Do the minimum dependency-optional work (A mechanics) to get `cargo check --target wasm32-unknown-unknown` reaching source and then green for the graph+temporal+query surface — **while drawing the module seam so the gated-in-memory engine is exactly the future `aletheiadb-core`.** The reason to start with A is *not* "B is churn" (B is actually strengthened by the already-clean `core/`); it is that **the `query ↔ storage/index` seam location is unproven**, and A lets us validate exactly where that boundary falls cheaply before committing to a crate split. C's facade is adopted at the `cdylib`/bindings stage (Phase 3), where isolating the wasm-bindgen surface — and excluding the CLI stack — in its own crate is genuinely cleaner.

## 6. The realistic v1 shape

- **Ephemeral, in-memory only.** `AletheiaDB::new()` (in-RAM) — persistence, WAL, index-persistence, cold storage compiled out. **No `open()` on wasm.** Durability is explicitly *not* offered in v1; the type surface says so.
- **Single-threaded execution, single HLC**, intended to run in one dedicated web worker so it never blocks the UI thread. `rayon` parallel iterators `cfg`-selected to serial on wasm. Multi-worker sharding is explicitly out of v1 (it needs a distributed-clock protocol — §9).
- **Clock:** the existing `time::now()` seam gains a wasm backend reading `js-sys`/`web-sys` (`Date.now()` wallclock, `performance.now()` monotonic), feeding the HLC. Native backend unchanged.
- **Vector search:** available in the *agent-memory* profile via a pure-Rust HNSW (`instant-distance`/`hnsw_rs`) behind a backend feature, swapped in through the same Phase-1 dependency surgery as the codec. The minimal *graph+temporal+query* profile (docs playground) can ship with vectors off; the agent-memory profile turns them on. (This corrects the earlier instinct to defer all vector search to Phase 4 — that would strand the agent-memory use case, §2.)
- **Compression:** kept ON via a pure-Rust bidirectional codec (`lz4_flex` / `miniz_oxide`); only the C `zstd` codec is gated out. Anchor+delta logic unchanged. Rationale: on the memory-capped target, compression is more valuable, not less (§9).
- **RNG:** `getrandom` `js` backend for the crypto/hashing that survives. Encryption compiles (pure-Rust AEAD); only its fs-touching at-rest path is gated out.
- **Crate output:** a `cdylib` in an `aletheiadb-wasm` facade (today the crate is rlib-only), never depending on the CLI/TUI stack, with a minimal `wasm-bindgen` smoke surface: create node → update node → as-of read. Compile-verified only (`cargo check --target wasm32-unknown-unknown`); no browser required for the spike.
- **Security:** the auth/RBAC key store (0600 `keys.json` on fs) has no home in a browser and is **absent-by-design** on wasm — auth is gated on `http-server`/`mcp-server` (off here), and the trust boundary is the user's own page.

## 7. Risks & edge cases as test cases

Each risk is written as the test that would catch its regression.

1. **Native build unaffected by wasm gating.** `cargo check --no-default-features --tests` and the full CI feature set stay green after every stage. (Guard against `cfg` mistakes leaking into native.)
2. **wasm frontier actually advances.** A CI job runs `cargo check --target wasm32-unknown-unknown -p aletheiadb-wasm` and must pass once Phase 3 lands; before then, a job asserts the build gets *past* the dependency layer (no getrandom/cxx/zstd-sys failure).
3. **HLC monotonicity on wasm (single worker).** Unit test: 10k successive `time::now()` calls through the wasm clock backend are non-decreasing and the HLC logical counter increments on same-wallclock ties.
4. **Cross-worker ordering is refused, not silently wrong.** Test/asserted invariant: v1 exposes no API that runs two engine instances against one logical dataset; any multi-worker path is gated out or returns an explicit "unsupported without a shared clock" error — never two independent HLCs writing one history.
5. **Ephemeral honesty.** Compile-fail (trybuild) test: `AletheiaDB::open(path)` does not exist in the wasm profile; only `new()` does. A doc test asserts data does not survive a dropped instance.
6. **History memory is bounded, not just interned strings.** Test: accumulating N versions under a configured RAM budget triggers a hot-version eviction/refusal policy (the tab-OOM guard) — distinct from and in addition to the interner string-count cap.
7. **Compression stays lossless with the pure-Rust codec.** Round-trip test: write history, reconstruct an old version, assert byte-identical to native — with the wasm codec (`lz4_flex`/`miniz_oxide`) substituted for zstd.
8. **Vector recall parity for the agent-memory profile.** Test: k-NN results from the pure-Rust HNSW match the native usearch results within an accepted recall tolerance on a fixed fixture.
9. **Serial-vs-parallel equivalence.** Property test: results of an operation that uses `rayon` natively are identical when run through the wasm serial path (same inputs → same output set/order guarantees).
10. **getrandom js gate does not regress native.** Native still uses the default backend; the `[target.'cfg(target_arch = "wasm32")']` table never affects x86_64.
11. **as-of read correctness on wasm.** The smoke example (create → update → as-of read of the pre-update value) is an integration test compiled for both native and wasm; native runs it, wasm compile-checks it.

## 8. Phased roadmap

Effort tiers are relative scope, not calendar estimates.

- **Phase 0 — getrandom (XS).** Add target-gated `getrandom` `js` feature. Clears Build-A's first blocker. Zero native impact. (Note: a later `getrandom 0.2 → 0.3` migration is *not* one line — 0.3 changed backend registration — so Phase 0 uses the `js` feature on 0.2, not a version bump.)
- **Phase 1 — dependency-optional surgery + call-site cfg (L, the gate).** Make `usearch`, `zstd`, `memmap2`, `redb`, `rayon`, and the CLI/TUI stack (`crossterm`/`comfy-table`) optional; introduce a `wasm`/`wasm-compat` feature that turns them off and selects alternatives — pure-Rust HNSW for vectors, a pure-Rust bidirectional codec for compression, serial iterators for `rayon`, in-memory storage for redb/memmap2. Because you cannot mark a dep optional without `cfg`-gating its call sites, this phase *is* partly source work at each dep boundary (the Phase-1/2 line is porous by nature); the mild de-risk is that the `rayon`/vector call sites concentrate in `index/vector/*`, already gated out. **Deliverable:** `cargo check --target wasm32-unknown-unknown --features wasm-compat --no-default-features` reaches AletheiaDB source and emits its *first* source errors — at which point the true source-site count (the concrete gated-site inventory) becomes measurable for the first time.
- **Phase 2 — source cfg + clock (M).** Gate the surviving `std::fs`/`std::thread`/`SystemTime`/scattered `Instant`/`mmap` sites; add the wasm backend to the existing `time::now()` seam; drive `cargo check --target wasm32-unknown-unknown` to green for the graph+temporal+query surface. (Clock lives here, not Phase 1, because Phase 1 compiles no source.)
- **Phase 3 — cdylib + facade + bindings + smoke (M).** Add the `aletheiadb-wasm` facade crate (`crate-type = ["cdylib"]`, never depending on the CLI stack), a minimal `wasm-bindgen` API (create/update/as-of read), and a compile-verified smoke example. Add the wasm CI job.
- **Phase 4 — persistence, vector & scale (L, post-v1).** OPFS (`createSyncAccessHandle`, worker-only synchronous `flush()`) / IndexedDB persistence for the browser — recovering real (if weaker) durability; WASI `fs` for server-side wasm; a per-request hydrate path for edge runtimes; multi-worker sharding *only* behind a distributed-clock protocol.

## 9. Honest hard parts

- **Whole-history-uncompressed-in-RAM vs the wasm ~4GB / 32-bit ceiling (top-tier).** The ephemeral profile removes *both* memory escape valves — cold-tier migration (redb gated out) and, if we were not careful, compression — on the one target with a hard, non-swappable single-linear-memory ceiling (~2–4GB). An append-only bi-temporal store that accumulates versions over a session, never migrates, and never compresses hits that wall precisely in the accumulate-over-a-session use it is sold for. Two consequences baked into §6: (a) **keep compression on** with a pure-Rust codec — gating zstd *off* is backwards for wasm; (b) a **hot-version RAM-budget/eviction policy is a v1 requirement**, not a Phase-4 nicety. The interner cap bounds string *count*, not version/history growth, so it does not address this.
- **Cross-worker HLC & transaction-time ordering (correctness, not perf).** "Web-worker-per-shard" is tempting for parallelism, but each worker has isolated linear memory and its own clock context and HLC logical counter — independent HLCs break the single monotonic transaction-time total order bi-temporal correctness depends on. v1 is therefore scoped to a single worker / single HLC; worker-per-shard requires a distributed-clock protocol (a genuine design item), which is why it is called out here and gated in §7 test #4 rather than sold as a benefit.
- **Bi-temporal durability without fsync.** On wasm v1 there is no fsync and no WAL. "Transaction time" still orders writes within a session, but *durability* is gone — a tab close loses everything. The semantics we keep are *temporal reasoning*, not *crash recovery*; this must be stated in the API, not implied. OPFS (Phase 4) recovers a weaker durability — with `createSyncAccessHandle` it is stronger than a naive async store, but still not a native fsync barrier — enough for agent memory, not for a system of record.
- **What temporal guarantees mean when ephemeral.** "As of 2024-01-01" is meaningful only over data recorded *this session* (until OPFS). The §2 framing is deliberately ranked so the fully-enabled docs playground leads and agent-memory is marked within-session-now / durable-pending-OPFS — the two sections must not drift back into implying persistence the v1 shape lacks.
- **Second build profile is ongoing tax.** Every future storage-touching change must keep the wasm profile green. The Approach-B core extraction is what makes this sustainable (the core physically cannot regress into `std::fs`), which is why A converges to B.

## Appendix A — dependency blocker table
See §3.2. Non-optional in `Cargo.toml`: `usearch`, `zstd`, `memmap2`, `redb`, `rayon`, `crossterm`, `comfy-table`, `clap`/`clap_complete`, `crc32fast`, `libc`, plus the non-optional crypto crates (`aes-gcm`/`chacha20poly1305`) and direct `rand` that over-determine `getrandom`. Optional but on-by-default and native: `simsimd`.

## Appendix B — raw build-failure excerpts

`wasm32-unknown-unknown --no-default-features` (first fatal, verbatim):
```
error: the wasm*-unknown-unknown targets are not supported by default, you may
       need to enable the "js" feature. For more information see:
       https://docs.rs/getrandom/#webassembly-support
   --> getrandom-0.2.17/src/lib.rs:346:9
error: could not compile `getrandom` (lib) due to 1 previous error
```
(Attribution is *not* in the log; determined separately via `cargo tree -i getrandom`: forced by `aes-gcm`/`chacha20poly1305` → `aead` → `crypto-common` → `rand_core`, by the `argon2`/`pbkdf2` KDF path → `crypto-common` → `rand_core`, and by the direct non-optional `rand = "0.8"` → `rand_core`.)

`wasm32-wasip1 --no-default-features` (first fatal, two native crates):
```
cxx v1.0.198:   clang++ --target=wasm32-wasip1 ... cxx.cc:
                fatal error: 'algorithm' file not found          (cxx  <- usearch)
zstd-sys v2.0.16: clang --target=wasm32-wasip1 ... zstd_v07.c:
                fatal error: 'bits/libc-header-start.h' file not found   (zstd-sys <- zstd; no WASI sysroot)
```

## Appendix C — the gated-site inventory, and why it is deferred
A concrete *per-site* gated inventory (the exact `std::fs`/`std::thread`/`time`/`mmap` lines needing `cfg`) **cannot be produced today**: both wasm builds die in the dependency layer before the compiler reaches a single line of AletheiaDB source (§3.1), so no source error is emitted to enumerate. This appendix is therefore satisfied in two parts: the **dependency-blocker inventory** (§3.2, Appendix A) now, and the **source-site inventory** at the Phase 1 gate — defined precisely as the point at which `cargo check --target wasm32-unknown-unknown --features wasm-compat` first reaches source and its errors become countable (§8 Phase 1 deliverable). The §3.3 grep counts are upper-bound estimates, not that inventory.

## Appendix D — always-compiled module surface under `--no-default-features`
`api`, `config`, `core`, `db`, `encryption`, `index`, `provenance_chain`, `query`, `storage`, `experimental`, `prelude`. `core/` is already dependency-clean (imports none of storage/index/encryption). The fs/thread/fsync concentration is in `storage` (WAL, index-persistence, cold, backup, rotation) and `encryption`'s at-rest path — the durability core the ephemeral profile compiles out. `query → {storage, index}` is the coupling that dictates where the Approach-B core seam must fall (§5).
