# 🔭 Vantage Spec: AletheiaDB CLI (The "Control" Dimension)

| Metadata | Details |
| :--- | :--- |
| **ID** | SPEC-006 |
| **Status** | 📝 Draft |
| **Owner** | Vantage (Product) |
| **Implementer** | Nova (Engineering) |
| **Priority** | P2 (Medium) |
| **Related Code** | `src/bin/cli.rs` (New) |

## 1. 👤 User Stories

> **As a** DevOps Engineer,
> **I want to** check the database health and trigger backups from a CI/CD pipeline,
> **So that** I can automate maintenance tasks without writing custom scripts.

> **As a** Backend Developer,
> **I want to** seed the database with initial data from a JSON file,
> **So that** I can quickly set up local development environments.

> **As a** Data Analyst,
> **I want to** run ad-hoc AQL queries and pipe the output to `jq`,
> **So that** I can inspect data without writing a full application.

## 2. 🧐 The "So What?" (Business Value)

Currently, interacting with AletheiaDB requires either writing Rust code (embedded) or using `curl` against the HTTP server (once implemented).

**The Gap:**
- **Usability**: Setting up a new instance is manual. There is no easy way to "just load data".
- **Observability**: Checking "is it running?" requires knowing the HTTP endpoint structure.
- **Automation**: Backups and maintenance require external tooling.

**ROI:**
- **Developer Experience (DX)**: Reduces "Time to Hello World" significantly.
- **Operations**: Standardizes how the database is managed in production.

## 3. ✅ Acceptance Criteria

### Functional Requirements

1.  **Global Flags**:
    -   `--host`: Server address (default: `127.0.0.1`).
    -   `--port`: Server port (default: `8080`).
    -   `--json`: Force JSON output for all commands.

2.  **Commands**:

    -   `health`:
        -   Pings the `/status` endpoint.
        -   Returns exit code 0 if healthy, 1 if unhealthy.
        -   Output: "✅ Healthy (Latency: 2ms)" or "❌ Unreachable".

    -   `query <AQL>`:
        -   Executes an AQL query string against the server.
        -   Output: Formatted table (default) or JSON (if `--json` is set).
        -   Example: `aletheia query "MATCH (n:Person) RETURN n LIMIT 5"`

    -   `import`:
        -   `--file <path>`: Path to a JSON/CSV file.
        -   `--format <auto|json|csv>`: Input format.
        -   Supports bulk loading of nodes and edges via the API.

    -   `backup`:
        -   `--out <path>`: Path to save the backup (snapshot).
        -   Triggers a server-side snapshot and downloads it (or instructs server to save to S3).

    -   `bench`:
        -   Runs a quick local benchmark (insert/query latency) to verify performance.

3.  **Error Handling**:
    -   Friendly error messages ("Connection refused" instead of stack trace).
    -   Exit codes must be POSIX compliant.

### Non-Functional Requirements
-   **Binary Size**: Optimized for distribution.
-   **Startup Time**: Instantaneous (< 50ms).

## 4. 🚫 Out of Scope (Phase 1)

-   **REPL**: Interactive shell with syntax highlighting. (Phase 2)
-   **TUI**: Text User Interface (like `k9s`). (Phase 3)
-   **User Management**: Managing users/roles (auth is out of scope for now).

## 5. 📝 Gap Analysis (Current vs. Spec)

| Feature | Current State | Required State | Action |
| :--- | :--- | :--- | :--- |
| **CLI Binary** | None | `aletheia-cli` | Create binary entry point |
| **Arguments** | N/A | Standard CLI Args | Implement argument parsing |
| **Client SDK** | Internal tests only | Reusable HTTP Client | Extract Client SDK |
| **Server: Import** | Single-node create only | Bulk Import Endpoint | Add batch endpoint to Server |
| **Server: Backup** | Manual recovery only | Snapshot Endpoint | Add snapshot trigger to Server |

## 6. 📅 Execution Plan

1.  **Skeleton**: Create the CLI binary structure.
2.  **SDK**: Implement a lightweight internal HTTP client to communicate with the server.
3.  **Implement Commands**:
    -   `health`: Connect to `/status`.
    -   `query`: Connect to `/query`.
    -   `import`: Implement file reading and batching logic.
4.  **Server Updates**: Ensure server supports required endpoints for Import/Backup.
5.  **Distribution**: Add binary to package configuration.
