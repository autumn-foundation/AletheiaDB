# DX Audit Report - Echo

## 1. 🔍 EXPERIENCE - The Walkthrough
**Scenario:** I am a new user trying to run the basic `AletheiaDB` examples sequentially from the `README.md` to see how it works.
**Action:** I copy-pasted the "Basic Graph Operations" example, ran it, and it worked perfectly! Then I moved to the next examples (like the "Time-Travel Queries" or "Configuration" example) which also use `AletheiaDB::new().unwrap()`.

## 2. 🚧 STUMBLE - The Friction Points
When I ran the second example, it immediately crashed with a terrifying error:
```
Warning: Failed to load manifest: Missing required index file: manifest.idx
Replaying 12 WAL entries from LSN 1
Error: Temporal(InvalidTimeRange { start: HybridTimestamp { wallclock: 1773781667467509, logical: 0 }, end: HybridTimestamp { wallclock: 1773781629065641, logical: 0 } })
```

"Why does running a fresh example crash with `InvalidTimeRange`?!"
It turns out `AletheiaDB::new()` implicitly saves data to disk at `./aletheiadb/wal`. So when I ran the second example, it loaded the state from the first example and caused a temporal collision! My filesystem got polluted and the basic examples just flat-out crashed when run one after another.

## 3. 📢 REPORT - The Complaint
**Title:** 🗣️ Echo: `AletheiaDB::new()` crashes with `InvalidTimeRange` on subsequent runs

* 🤦 **The Confusion:** Tried to run multiple examples sequentially from the README. The first one worked, but all subsequent examples using `AletheiaDB::new()` crashed with `InvalidTimeRange` errors or complained about missing index files.
* 🕵️ **The Reality:** Turns out `AletheiaDB::new()` writes data to `./aletheiadb/wal` by default. The second example loaded the first example's state and corrupted the run. The README *does* have a tiny warning about this, but users don't read warnings—they copy-paste code!
* 💡 **The Fix:** The examples in the README should use a temporary directory configuration or explicitly clear the database state, OR `AletheiaDB::new()` should be an in-memory database by default so it's safe to use without polluting the user's filesystem and crashing their sequential tests.

## 4. 🧪 VERIFY - The "idiot proofing"
If the examples are updated to clear their state or use an in-memory config, a user should be able to run `cargo run --example basic` and `cargo run --example time_travel` back-to-back without errors or manually deleting folders.
