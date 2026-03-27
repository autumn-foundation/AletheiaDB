# 🗣️ Echo: Missing OPENAI_API_KEY environment variable in Embedding Example

🤦 **The Confusion:**
I copy-pasted the "Embedding Generation" example from the README into a new project and ran `cargo run`. It compiled perfectly, but immediately crashed at runtime with `Error: ConfigError("OPENAI_API_KEY environment variable not set")`. There was no mention of needing this variable in the README snippet.

🕵️ **The Reality:**
The example uses `OpenAIConfig::from_env(...)` which silently depends on an environment variable (`OPENAI_API_KEY`) being present. Since it's a "Quick Start" example, users shouldn't have to guess what environment variables are required to make it run.

💡 **The Fix:**
Add a comment in the README code block explicitly showing that the `OPENAI_API_KEY` environment variable must be set, and add a small note above the code block.
