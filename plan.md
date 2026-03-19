1. **Append DX Report to Echo's Journal**
   - Append a formatted report to `.jules/echo.md` identifying the failure when running the `execute_aql` example from the README.
   - The report will include the `EXPERIENCE` (trying to run the AQL `AS OF` example), the `STUMBLE` (getting a `Query(InvalidParameter)` error because it expected microseconds, not an ISO string), and a `REPORT` requesting a Docs Fix or an API fix so that strings work.
   - Wait, should I submit an issue or PR with only a markdown file? The prompt says "Create an Issue (or PR with a 'Docs Fix' request)". I should create a file with the report or submit a PR containing the markdown report in `.jules/echo.md`.
2. **Commit and Submit PR**
   - I am not fixing the code or docs. I am just submitting the DX report!
   - Complete pre-commit steps to ensure proper testing, verification, review, and reflection are done.
   - Submit the PR with the title '🗣️ Echo: Getting Started example is broken'.

Wait, let's look at `.jules/echo.md` to see its format.
