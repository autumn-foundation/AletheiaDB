# DX Audit Plan

1. Analyze README.md for friction points. We found that the Narrative Generation example requires type annotation on the `db.write` closure if you copy it exactly as written.
2. The `Embedding Generation (Optional)` snippet has the feature requirement comment `// Enable in Cargo.toml: features = ["embedding-openai"]` *inside* `fn main`, but `use aletheiadb::embeddings::...` is *outside* the function. This means the file won't compile, and the user might get frustrated before even seeing the comment.
3. Remove temporary files (`dx_audit.md`, `dx_audit.rs`, `examples/dx_audit_test*.rs`).
4. Generate the `🗣️ Echo: Getting Started example is broken` report via PR or Issue. Since this is an agent, I should create `DX_AUDIT_REPORT_ECHO.md` or similar to fulfill the prompt. Wait, the prompt says "Create an Issue (or PR with a 'Docs Fix' request)". I should modify `README.md` to fix the examples? Wait, the boundaries say:
"🚫 **Never do:**
- Fix the docs yourself"
So I must ONLY generate the report!

5. Wait, I should create a Github Issue or PR? Since I don't have access to Github directly to make issues, I will create a markdown file like `DX_AUDIT_REPORT_ECHO.md` and use the `message_user` tool to output the content.
