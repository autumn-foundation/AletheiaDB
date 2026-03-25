# 🗣️ Echo: Getting Started example is broken

## 1. 🔍 EXPERIENCE - The Walkthrough
**Scenario:** "I am a new user trying to run the examples in the README."
**Action:** Try to use the API based *only* on the public docs/examples in `README.md`.

## 2. 🚧 STUMBLE - The Friction Points
- The `execute_aql` function throws an error because it expects a timestamp in microseconds since epoch instead of an ISO 8601 string.
- The Embedding Generation code requires an `OPENAI_API_KEY` to run, which is not stated in the text surrounding the example.
- There are unused variable warnings for `result` in the Transactions example and `db` in the Production Observability example.

## 3. 📢 REPORT - The Complaint
- 🤦 **The Confusion:** "Tried to run the `execute_aql` example. Compiler said `Invalid timestamp '2024-01-15T10:00:00Z'. Expected microseconds since epoch.`"
- 🕵️ **The Reality:** "Turns out the API only accepts timestamps in microseconds since epoch as an integer."
- 💡 **The Fix:** "Change the example query to use a valid microsecond timestamp integer (e.g., `1705312800000000`)."

- 🤦 **The Confusion:** "Tried to run the Embedding Generation example. Got `Error: ConfigError(\"OPENAI_API_KEY environment variable not set\")`."
- 🕵️ **The Reality:** "The `OpenAIConfig::from_env` expects `OPENAI_API_KEY` to be set in the environment, but it's not documented."
- 💡 **The Fix:** "Add a comment specifying `// Requires OPENAI_API_KEY environment variable`."

- 🤦 **The Confusion:** "Got compiler warnings for unused variables in the Transactions and Observability examples."
- 🕵️ **The Reality:** "The variables `result` and `db` are assigned but never used."
- 💡 **The Fix:** "Prefix them with an underscore, e.g., `_result` and `_db`."

## 4. 🧪 VERIFY - The "idiot proofing"
Verified the fixes locally by compiling and executing the examples.
