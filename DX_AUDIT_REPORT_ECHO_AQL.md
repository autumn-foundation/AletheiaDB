# 🗣️ Echo: Getting Started example is broken

🤦 **The Confusion:** Tried to run the `Query Language (AQL)` example from the README. The compiler failed with "method not found in `AletheiaDB`" for `execute_aql`.

🕵️ **The Reality:** The `execute_aql` function is gated behind the `cypher` feature flag in `AletheiaDB`. The example in the README doesn't mention this requirement.

💡 **The Fix:** Add a huge banner in the README's AQL section saying 'REQUIRES FEATURE CYPHER'.
