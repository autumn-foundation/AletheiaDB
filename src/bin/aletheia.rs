//! AletheiaDB command line interface.
//!
//! This binary provides:
//! - Direct local graph operations (inspired by MCP tool semantics)
//! - Daemon management for the HTTP server process

use std::env;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(feature = "serde")]
use aletheiadb::provenance_chain::ChainHead;
use aletheiadb::provenance_chain::{ChainVerification, EntityKind};
use aletheiadb::{
    AletheiaDB, Edge, EdgeId, GLOBAL_INTERNER, Node, NodeId, PropertyMap, PropertyMapBuilder,
    PropertyValue,
};

const DEFAULT_PID_FILE: &str = ".aletheia/daemon.pid";
const DEFAULT_LOG_FILE: &str = ".aletheia/daemon.log";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 1963;
const SERVER_BIN_NAME: &str = "aletheia-server";

#[derive(Debug, Clone)]
struct DaemonMetadata {
    pid: u32,
    server_exe: PathBuf,
}

/// The main entry point for the AletheiaDB CLI.
///
/// This simply wraps `run()` and handles exiting with the correct status code
/// if an error occurs.
fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

/// Parses CLI arguments and routes to the appropriate subcommand handler.
///
/// # Returns
/// - `Ok(())` if the command succeeded.
/// - `Err(String)` with an error message if something failed or syntax was wrong.
fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("demo") => handle_demo(args.collect()),
        Some("node") => handle_node(args.collect()),
        Some("edge") => handle_edge(args.collect()),
        Some("traverse") => handle_traverse(args.collect()),
        Some("daemon") => handle_daemon(args.collect()),
        Some("backup") => handle_backup(args.collect()),
        Some("restore") => handle_restore(args.collect()),
        Some("verify") => handle_verify(args.collect()),
        Some("import") => handle_import(args.collect()),
        #[cfg(feature = "audit-export")]
        Some("audit-keygen") => audit::handle_keygen(args.collect()),
        #[cfg(feature = "audit-export")]
        Some("audit-export") => audit::handle_export(args.collect()),
        #[cfg(feature = "audit-export")]
        Some("audit-verify") => audit::handle_verify(args.collect()),
        #[cfg(feature = "audit-export")]
        Some("audit-render") => audit::handle_render(args.collect()),
        Some("help") | Some("--help") | Some("-h") | None => {
            print_usage();
            Ok(())
        }
        Some(cmd) => Err(format!("unknown command '{cmd}'")),
    }
}

/// Prints the CLI usage instructions to standard output.
fn print_usage() {
    println!(
        "AletheiaDB CLI\n\n\
Usage:\n\
  aletheia demo\n\
  aletheia node create <label> [--properties '{{\"k\":\"v\"}}']\n\
  aletheia node get <node_id>\n\
  aletheia edge create <source_id> <target_id> <label> [--properties '{{\"k\":\"v\"}}']\n\
  aletheia edge get <edge_id>\n\
  aletheia traverse <start_node_id> <edge_label> [--direction outgoing|incoming|both]\n\
  aletheia daemon start [--pid-file PATH] [--log-file PATH] [--host HOST] [--port PORT]\n\
  aletheia daemon stop [--pid-file PATH]\n\
  aletheia daemon status [--pid-file PATH]\n\
  aletheia backup <output_path>\n\
  aletheia restore <input_path>\n\
  aletheia verify [--entity <node|edge>:<id>] [--export-head PATH] [--against PATH] [--json]\n\
  aletheia import --format neo4j-csv --nodes <f>... --relationships <f>... [options]\n\
  aletheia audit-keygen <key_file>\n\
  aletheia audit-export <node|edge> <id> --key <key_file> --out <path> [--db-id ID] [--redact k1,k2]\n\
  aletheia audit-verify <artifact_path> [--public-key HEX]\n\
  aletheia audit-render <artifact_path>\n\
\nCommands map to core MCP-style graph operations while using local storage.\n\
\nGetting started:\n\
  demo    — Boot a seeded, ephemeral bi-temporal graph and run a guided tour of\n\
            current-state, AS OF time-travel, traversal, and history queries.\n\
            No data dir, no server, no network — just `aletheia demo`.\n\
\nBackup / Restore:\n\
  backup  — Write a portable .albk artifact capturing full bi-temporal state.\n\
            Opens the database via ALETHEIADB_CONFIG or ALETHEIADB_DATA_DIR.\n\
  restore — Restore a .albk artifact into ALETHEIADB_DATA_DIR (must be empty).\n\
\nProvenance hash chain (Issue #3351):\n\
  verify  — Verify the tamper-evident provenance hash chain over recorded history.\n\
            Default: full-chain verify. --entity <node|edge>:<id> scopes to one entity.\n\
            --export-head PATH writes the current head anchor (JSON) for offsite storage.\n\
            --against PATH verifies the chain append-only-extends a previously exported\n\
            head (fork/rollback detection). --json emits machine-readable output.\n\
            Requires the chain to be enabled for the opened data dir (via an\n\
            ALETHEIADB_CONFIG TOML carrying `[chain] enabled = true`); exits non-zero\n\
            if the chain is disabled or verification fails.\n\
\nImport (Issue #3356):\n\
  import  — Load a Neo4j CSV export (neo4j-admin / apoc.export.csv typed headers)\n\
            into the database, emitting a machine-readable fidelity report.\n\
            Requires --features import. Options:\n\
              --format neo4j-csv         (required; binary .dump is unsupported)\n\
              --nodes <file>             node CSV file (repeatable)\n\
              --relationships <file>     relationship CSV file (repeatable)\n\
              --array-delimiter <char>   array/multi-label delimiter (default ;)\n\
              --delimiter <char>         field delimiter (default ,)\n\
              --quote <char>             quote char (default \")\n\
              --vector-property <name>   numeric[] -> vector (repeatable, opt-in)\n\
              --valid-from-property <n>  date/datetime column -> valid_from\n\
              --label-strategy <s>       first|concat|property (default first)\n\
              --on-error <mode>          abort|skip (default abort)\n\
              --strict-types             reject unsupported type headers\n\
              --report <path>            also write the fidelity report JSON here\n\
\nSigned audit export (Issue #3358):\n\
  audit-keygen — Generate an Ed25519 signing key (0600) and print its public key.\n\
  audit-export — Sign an entity's full bi-temporal history into a portable artifact.\n\
  audit-verify — Verify an artifact OFFLINE (no database) with the signer's public key.\n\
  audit-render — Render an artifact as a human-readable chronology.\n"
    );
}

/// `aletheia backup <output_path>` — create a portable backup artifact.
fn handle_backup(args: Vec<String>) -> Result<(), String> {
    let path = args
        .first()
        .ok_or_else(|| "usage: aletheia backup <output_path>".to_string())?;
    let db = open_db()?;
    let summary = db
        .backup(std::path::Path::new(path))
        .map_err(|e| format!("backup failed: {e}"))?;
    let value = serde_json::json!({
        "ok": true,
        "bytes_written": summary.bytes_written,
        "node_versions": summary.node_versions,
        "edge_versions": summary.edge_versions,
        "current_node_count": summary.current_node_count,
        "current_edge_count": summary.current_edge_count,
        "source_lsn": summary.source_lsn,
    });
    let rendered =
        serde_json::to_string(&value).map_err(|e| format!("failed to render JSON output: {e}"))?;
    println!("{rendered}");
    Ok(())
}

/// `aletheia restore <input_path>` — restore a backup artifact into `ALETHEIADB_DATA_DIR`.
fn handle_restore(args: Vec<String>) -> Result<(), String> {
    let path = args
        .first()
        .ok_or_else(|| "usage: aletheia restore <input_path>".to_string())?;
    let data_dir = env::var("ALETHEIADB_DATA_DIR")
        .map(PathBuf::from)
        .map_err(|_| {
            "ALETHEIADB_DATA_DIR must be set to restore into a durable directory".to_string()
        })?;
    AletheiaDB::restore_to_data_dir(std::path::Path::new(path), &data_dir)
        .map_err(|e| format!("restore failed: {e}"))?;
    println!("{}", restore_success_json(&data_dir)?);
    Ok(())
}

/// `aletheia verify` — verify the tamper-evident provenance hash chain
/// (Issue #3351).
///
/// Modes, resolved in precedence order:
/// - `--export-head PATH`: export the current chain head anchor as JSON to
///   `PATH` (for offsite storage; feed it back later via `--against`).
/// - `--against PATH`: load a previously exported head and prove the current
///   chain append-only-extends it (rollback/fork detection).
/// - `--entity <node|edge>:<id>`: verify only one entity's contribution.
/// - (default): full-chain verify from genesis.
///
/// `--json` emits machine-readable output. Verification failure (or a
/// disabled chain) exits non-zero.
///
/// The database is opened via [`open_db`], which honours `ALETHEIADB_CONFIG`
/// (a TOML file that can enable the chain with `[chain] enabled = true`) or
/// `ALETHEIADB_DATA_DIR`. Note that opening by data dir alone does NOT enable
/// the chain — a chain-enabled TOML config is required for `verify` to have a
/// chain to check.
fn handle_verify(args: Vec<String>) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let db = open_db()?;

    // Export-head mode takes precedence: it is an explicit export action.
    // Exporting/importing a chain head anchor serializes `ChainHead` via
    // `serde_json`, so it is only available when the `serde` feature is
    // compiled in.
    #[cfg(feature = "serde")]
    if let Some(path) = arg_value(&args, "--export-head") {
        let head = db.export_chain_head().map_err(chain_error_hint)?;
        write_chain_head(&head, &path)?;
        println!("{}", render_head_export(&head, &path, json)?);
        return Ok(());
    }

    // Against-anchor mode: prove append-only extension of a stored head.
    #[cfg(feature = "serde")]
    if let Some(path) = arg_value(&args, "--against") {
        let anchor = read_chain_head(&path)?;
        let result = db.verify_chain_against(&anchor).map_err(chain_error_hint)?;
        return finish_verification(&result, "anchor", json);
    }

    // Without `serde`, the chain-head export/compare CLI is unavailable. Report
    // a clean error (non-zero exit via `Err`) instead of silently ignoring the
    // flags or leaving a compile error, mirroring how other feature-gated CLI
    // paths surface unavailability. The plain `verify` path below still works.
    #[cfg(not(feature = "serde"))]
    if arg_value(&args, "--export-head").is_some() || arg_value(&args, "--against").is_some() {
        return Err(
            "chain head export/compare requires the `serde` feature (rebuild with \
             --features serde)"
                .to_string(),
        );
    }

    // Entity-scoped mode.
    if let Some(spec) = arg_value(&args, "--entity") {
        let (kind, id) = parse_entity_arg(&spec)?;
        let result = db.verify_entity_chain(kind, id).map_err(chain_error_hint)?;
        return finish_verification(&result, "entity", json);
    }

    // Default: full-chain verify.
    let result = db.verify_chain().map_err(chain_error_hint)?;
    finish_verification(&result, "full", json)
}

/// Turn a chain API error (notably "chain not enabled") into a CLI-friendly
/// message with remediation guidance.
fn chain_error_hint(e: impl std::fmt::Display) -> String {
    format!(
        "{e}\n\
         hint: `aletheia verify` requires the provenance hash chain to be enabled for the \
         opened database. Open it via ALETHEIADB_CONFIG pointing at a TOML config that \
         carries a `[chain]` section with `enabled = true`."
    )
}

/// Parse an `--entity <node|edge>:<id>` argument into its kind and id.
fn parse_entity_arg(spec: &str) -> Result<(EntityKind, u64), String> {
    let (kind_str, id_str) = spec.split_once(':').ok_or_else(|| {
        format!("invalid --entity '{spec}': expected '<node|edge>:<id>' (e.g. node:42)")
    })?;
    let kind = match kind_str.trim().to_ascii_lowercase().as_str() {
        "node" => EntityKind::Node,
        "edge" => EntityKind::Edge,
        other => {
            return Err(format!(
                "invalid --entity kind '{other}': expected 'node' or 'edge'"
            ));
        }
    };
    let id = id_str
        .trim()
        .parse::<u64>()
        .map_err(|e| format!("invalid --entity id '{id_str}': {e}"))?;
    Ok((kind, id))
}

/// Render a verification result and return non-zero (via `Err`) on failure.
///
/// The full result is printed to stdout in both success and failure cases so a
/// caller always sees the details; on failure a terse `Err` drives the
/// process exit code to non-zero (main prints it to stderr).
fn finish_verification(result: &ChainVerification, scope: &str, json: bool) -> Result<(), String> {
    println!("{}", render_verification(result, scope, json)?);
    if result.passed {
        Ok(())
    } else {
        Err(format!(
            "provenance chain verification FAILED ({scope}){}",
            result
                .earliest_broken_seq
                .map(|s| format!(" at seq {s}"))
                .unwrap_or_default()
        ))
    }
}

/// Render a [`ChainVerification`] as human-readable text or JSON.
fn render_verification(
    result: &ChainVerification,
    scope: &str,
    json: bool,
) -> Result<String, String> {
    if json {
        let value = serde_json::json!({
            "scope": scope,
            "passed": result.passed,
            "head_seq": result.head_seq,
            "head_digest": result.head_digest_hex,
            "earliest_broken_seq": result.earliest_broken_seq,
            "reason": result.reason,
            "transactions_checked": result.transactions_checked,
        });
        return serde_json::to_string_pretty(&value)
            .map_err(|e| format!("failed to render JSON output: {e}"));
    }

    let status = if result.passed { "PASS" } else { "FAIL" };
    let mut out = String::new();
    out.push_str(&format!(
        "Provenance chain verification: {status} (scope: {scope})\n"
    ));
    out.push_str(&format!("  head seq:             {}\n", result.head_seq));
    out.push_str(&format!(
        "  head digest:          {}\n",
        result.head_digest_hex
    ));
    out.push_str(&format!(
        "  transactions checked: {}",
        result.transactions_checked
    ));
    if !result.passed {
        if let Some(seq) = result.earliest_broken_seq {
            out.push_str(&format!("\n  earliest broken seq:  {seq}"));
        }
        if let Some(reason) = &result.reason {
            out.push_str(&format!("\n  reason:               {reason}"));
        }
    }
    Ok(out)
}

/// Render the confirmation for an `--export-head` action.
#[cfg(feature = "serde")]
fn render_head_export(head: &ChainHead, path: &str, json: bool) -> Result<String, String> {
    if json {
        let value = serde_json::json!({
            "ok": true,
            "path": path,
            "seq": head.seq,
            "digest": aletheiadb::provenance_chain::to_hex(&head.digest),
        });
        return serde_json::to_string(&value)
            .map_err(|e| format!("failed to render JSON output: {e}"));
    }
    Ok(format!(
        "Exported chain head anchor to {path}\n  seq:    {}\n  digest: {}",
        head.seq,
        aletheiadb::provenance_chain::to_hex(&head.digest)
    ))
}

/// Serialize a [`ChainHead`] to a JSON file (pretty, digests as hex).
#[cfg(feature = "serde")]
fn write_chain_head(head: &ChainHead, path: &str) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(head)
        .map_err(|e| format!("failed to serialize chain head: {e}"))?;
    fs::write(path, bytes).map_err(|e| format!("failed to write chain head to '{path}': {e}"))
}

/// Load a [`ChainHead`] previously exported to a JSON file.
#[cfg(feature = "serde")]
fn read_chain_head(path: &str) -> Result<ChainHead, String> {
    let bytes =
        fs::read(path).map_err(|e| format!("failed to read chain head from '{path}': {e}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("'{path}' is not a valid exported chain head: {e}"))
}

/// `aletheia import` — load an external graph export (Issue #3356).
///
/// v1 supports the Neo4j CSV export convention (`--format neo4j-csv`). A binary
/// Neo4j `.dump` archive and the APOC Cypher-script dump are unsupported and
/// rejected with an actionable message (documented follow-ups).
#[cfg(feature = "import")]
fn handle_import(args: Vec<String>) -> Result<(), String> {
    use aletheiadb::api::import::{FailureMode, LabelStrategy, Neo4jCsvOptions};

    let format = arg_value(&args, "--format").unwrap_or_else(|| "neo4j-csv".to_string());
    let nodes = arg_values(&args, "--nodes");
    let rels = arg_values(&args, "--relationships");

    // Reject binary dump / Cypher-script inputs with a clear pointer to CSV.
    for path in nodes.iter().chain(rels.iter()) {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".dump") {
            return Err(
                "neo4j binary dump import is not supported; export to CSV with \
                'neo4j-admin database import' headers or apoc.export.csv"
                    .to_string(),
            );
        }
        if lower.ends_with(".cypher") {
            return Err(
                "neo4j Cypher-script dump import is a documented follow-up; \
                use CSV export (neo4j-admin database import / apoc.export.csv)"
                    .to_string(),
            );
        }
    }

    match format.as_str() {
        "neo4j-csv" | "neo4j" => {}
        "neo4j-dump" | "dump" => {
            return Err(
                "neo4j binary dump import is not supported; export to CSV with \
                'neo4j-admin database import' headers or apoc.export.csv"
                    .to_string(),
            );
        }
        other => {
            return Err(format!(
                "unsupported import format '{other}'; supported: neo4j-csv"
            ));
        }
    }

    if nodes.is_empty() && rels.is_empty() {
        return Err(
            "usage: aletheia import --format neo4j-csv --nodes <file>... \
            [--relationships <file>...] [options]"
                .to_string(),
        );
    }

    let mut opts = Neo4jCsvOptions::new();
    if let Some(d) = arg_value(&args, "--array-delimiter") {
        opts.array_delimiter = single_char(&d, "--array-delimiter")?;
    }
    if let Some(d) = arg_value(&args, "--delimiter") {
        opts.delimiter = single_byte(&d, "--delimiter")?;
    }
    if let Some(q) = arg_value(&args, "--quote") {
        opts.quote = single_byte(&q, "--quote")?;
    }
    for name in arg_values(&args, "--vector-property") {
        opts.vector_properties.insert(name);
    }
    if let Some(name) = arg_value(&args, "--valid-from-property") {
        opts.valid_from_property = Some(name);
    }
    if let Some(strategy) = arg_value(&args, "--label-strategy") {
        opts.label_strategy = match strategy.as_str() {
            "first" => LabelStrategy::First,
            "concat" => LabelStrategy::Concat,
            "property" => LabelStrategy::Property,
            other => {
                return Err(format!(
                    "invalid --label-strategy '{other}', expected first|concat|property"
                ));
            }
        };
    }
    if args.iter().any(|a| a == "--strict-types") {
        opts.strict_types = true;
    }
    let failure_mode = match arg_value(&args, "--on-error").as_deref() {
        None | Some("abort") => FailureMode::Abort,
        Some("skip") => FailureMode::SkipAndReport,
        Some(other) => {
            return Err(format!("invalid --on-error '{other}', expected abort|skip"));
        }
    };

    let node_paths: Vec<PathBuf> = nodes.iter().map(PathBuf::from).collect();
    let rel_paths: Vec<PathBuf> = rels.iter().map(PathBuf::from).collect();

    let db = open_db()?;
    let mut importer = db.import().failure_mode(failure_mode);
    let report = importer
        .neo4j_import_csv(&node_paths, &rel_paths, &opts)
        .map_err(|e| format!("import failed: {e}"))?;

    let value =
        serde_json::to_value(&report).map_err(|e| format!("failed to render report JSON: {e}"))?;
    if let Some(report_path) = arg_value(&args, "--report") {
        let rendered = serde_json::to_string_pretty(&value)
            .map_err(|e| format!("failed to render report JSON: {e}"))?;
        fs::write(&report_path, rendered)
            .map_err(|e| format!("failed to write report to '{report_path}': {e}"))?;
    }
    print_json_pretty(&value)
}

/// Stub `import` handler when the `import` feature is not compiled in.
#[cfg(not(feature = "import"))]
fn handle_import(_args: Vec<String>) -> Result<(), String> {
    Err("the 'import' subcommand requires building with --features import".to_string())
}

/// Collect every value following each occurrence of `flag` (repeatable flags).
#[cfg(feature = "import")]
fn arg_values(args: &[String], flag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut iter = args.iter();
    while let Some(token) = iter.next() {
        if token == flag
            && let Some(value) = iter.next()
        {
            out.push(value.clone());
        }
    }
    out
}

/// Parse a single-character CLI argument into a `char`.
#[cfg(feature = "import")]
fn single_char(value: &str, flag: &str) -> Result<char, String> {
    let mut chars = value.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err(format!("{flag} must be a single character, got '{value}'")),
    }
}

/// Parse a single-ASCII-byte CLI argument (field delimiter / quote).
#[cfg(feature = "import")]
fn single_byte(value: &str, flag: &str) -> Result<u8, String> {
    let bytes = value.as_bytes();
    if bytes.len() == 1 {
        Ok(bytes[0])
    } else {
        Err(format!(
            "{flag} must be a single ASCII character, got '{value}'"
        ))
    }
}

/// `aletheia demo` — boot a seeded, ephemeral bi-temporal graph and print a
/// guided tour of showcase queries (Issue #3380 AC3).
///
/// This mirrors the seed + guided-query flow in `examples/demo.rs` so the
/// one-command CLI experience stays in sync with the Rust-native example and
/// the CI behavior guard (`tests/quickstart_demo.rs`). It requires no data
/// directory, no server, and no network: the database is ephemeral
/// (`AletheiaDB::new()`), so nothing is written to disk.
///
/// Any surplus arguments are ignored; the demo is a zero-configuration,
/// one-shot showcase.
fn handle_demo(_args: Vec<String>) -> Result<(), String> {
    let db = AletheiaDB::new().map_err(|e| format!("failed to initialize demo database: {e}"))?;
    run_demo(&db)
}

/// Builds a small string-valued `PropertyMap` for the demo seed. Keeps the
/// seeding code legible without depending on the `properties!` macro.
fn demo_props(pairs: &[(&str, &str)]) -> PropertyMap {
    let mut builder = PropertyMapBuilder::new();
    for (key, value) in pairs {
        builder = builder.insert(key, PropertyValue::string(*value));
    }
    builder.build()
}

/// Renders a `PropertyValue` for human-readable demo output (no Debug leakage
/// for the common scalar cases).
fn demo_display_value(value: &PropertyValue) -> String {
    match value {
        PropertyValue::String(s) => s.to_string(),
        PropertyValue::Int(i) => i.to_string(),
        PropertyValue::Float(f) => f.to_string(),
        PropertyValue::Bool(b) => b.to_string(),
        other => format!("{other:?}"),
    }
}

/// Extracts a string-valued property from a node, or a placeholder if absent.
fn demo_prop_str(node: &Node, key: &str) -> String {
    node.properties
        .get(key)
        .map(demo_display_value)
        .unwrap_or_else(|| "<none>".to_string())
}

/// Seeds a small story-driven bi-temporal dataset into `db` and prints a guided
/// sequence of showcase queries: a current-state lookup, an `AS OF`
/// point-in-time lookup, a graph traversal, and the full version history of a
/// node. Tells the same story as `examples/demo.rs` (the query ordering and
/// filler count differ; both flows are exercised in CI).
///
/// Extracted from [`handle_demo`] so the seed + query flow is unit-testable
/// against an injected database instance.
fn run_demo(db: &AletheiaDB) -> Result<(), String> {
    // The `update_node` transaction method lives on the `WriteOps` trait.
    use aletheiadb::api::WriteOps;

    // A small, deterministic filler cohort so the graph is more than a toy of a
    // few nodes, while keeping the one-shot CLI demo well under a second.
    const FILLER_ENGINEERS: usize = 20;

    println!("════════════════════════════════════════════════════════");
    println!("  AletheiaDB — bi-temporal graph demo");
    println!("  Ephemeral demo data (in-memory; nothing written to disk)");
    println!("════════════════════════════════════════════════════════\n");

    // ── Seed the graph ──────────────────────────────────────────────────────
    let company = db
        .create_node("Company", demo_props(&[("name", "Aletheia Labs")]))
        .map_err(|e| format!("failed to seed Company: {e}"))?;

    let alice = db
        .create_node(
            "Person",
            demo_props(&[("name", "Alice"), ("title", "Engineer")]),
        )
        .map_err(|e| format!("failed to seed Alice: {e}"))?;
    let bob = db
        .create_node(
            "Person",
            demo_props(&[("name", "Bob"), ("title", "Engineer")]),
        )
        .map_err(|e| format!("failed to seed Bob: {e}"))?;
    let carol = db
        .create_node(
            "Person",
            demo_props(&[("name", "Carol"), ("title", "Designer")]),
        )
        .map_err(|e| format!("failed to seed Carol: {e}"))?;

    db.create_edge(alice, company, "WORKS_AT", PropertyMap::new())
        .map_err(|e| format!("failed to seed Alice WORKS_AT: {e}"))?;
    db.create_edge(bob, company, "WORKS_AT", PropertyMap::new())
        .map_err(|e| format!("failed to seed Bob WORKS_AT: {e}"))?;
    db.create_edge(carol, company, "WORKS_AT", PropertyMap::new())
        .map_err(|e| format!("failed to seed Carol WORKS_AT: {e}"))?;
    db.create_edge(alice, bob, "KNOWS", PropertyMap::new())
        .map_err(|e| format!("failed to seed Alice KNOWS Bob: {e}"))?;
    db.create_edge(bob, carol, "KNOWS", PropertyMap::new())
        .map_err(|e| format!("failed to seed Bob KNOWS Carol: {e}"))?;

    let mut previous: Option<NodeId> = None;
    for i in 0..FILLER_ENGINEERS {
        let person = db
            .create_node(
                "Person",
                demo_props(&[("name", &format!("Engineer {i}")), ("title", "Engineer")]),
            )
            .map_err(|e| format!("failed to seed filler engineer {i}: {e}"))?;
        db.create_edge(person, company, "WORKS_AT", PropertyMap::new())
            .map_err(|e| format!("failed to seed filler WORKS_AT: {e}"))?;
        if let Some(prev) = previous {
            db.create_edge(prev, person, "KNOWS", PropertyMap::new())
                .map_err(|e| format!("failed to seed filler KNOWS: {e}"))?;
        }
        previous = Some(person);
    }

    // Capture the founding moment *after* the initial hire, so a query "as of
    // founding" sees Alice as an Engineer.
    let t_founding = aletheiadb::time::now();

    // The story unfolds: Alice is promoted twice. Each update creates a new
    // bi-temporal version; the old versions are preserved and stay queryable.
    // A tiny sleep between writes guarantees strictly-later transaction
    // timestamps so point-in-time reads resolve to distinct versions.
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.write(|tx| {
        tx.update_node(
            alice,
            demo_props(&[("name", "Alice"), ("title", "Staff Engineer")]),
        )
    })
    .map_err(|e| format!("failed to promote Alice to Staff Engineer: {e}"))?;
    std::thread::sleep(std::time::Duration::from_millis(2));
    db.write(|tx| tx.update_node(alice, demo_props(&[("name", "Alice"), ("title", "CTO")])))
        .map_err(|e| format!("failed to promote Alice to CTO: {e}"))?;

    println!(
        "Seeded {} nodes and {} edges (a Company \"Aletheia Labs\" plus its people)\n\
         with genuine bi-temporal version history.\n",
        db.node_count(),
        db.edge_count()
    );

    // ── Query 1: current-state lookup ───────────────────────────────────────
    println!("── Query 1 of 4: Current-state lookup ──");
    println!("  db.get_node(alice_id)");
    let alice_now = db
        .get_node(alice)
        .map_err(|e| format!("current-state lookup of Alice failed: {e}"))?;
    let title_now = demo_prop_str(&alice_now, "title");
    println!("  → Alice is currently: {title_now}");
    println!("  what you just saw: the *latest* fact — sub-microsecond, no temporal overhead.\n");

    // ── Query 2: AS OF point-in-time (the differentiator) ───────────────────
    println!("── Query 2 of 4: Time-travel — AS OF the founding day ──");
    println!("  db.get_node_at_time(alice_id, t_founding, t_founding)");
    let alice_founding = db
        .get_node_at_time(alice, t_founding, t_founding)
        .map_err(|e| format!("AS OF point-in-time lookup of Alice failed: {e}"))?;
    let title_founding = demo_prop_str(&alice_founding, "title");
    println!("  → On founding day, Alice was: {title_founding}");
    println!(
        "  what you just saw: the SAME node, a different answer — \
         \"{title_founding}\" then vs \"{title_now}\" now.\n"
    );

    // Guard the demo's headline invariant: the AS OF reconstruction must return
    // the *founding-day* fact, not silently regress to current state. Uses the
    // demo's `Result<(), String>` convention rather than a panic to match the
    // CLI error style (cf. the `assert_ne!` guard in examples/demo.rs:140).
    if title_founding == title_now {
        return Err(format!(
            "demo invariant broken: AS OF returned current state ({title_now})"
        ));
    }

    // ── Query 3: a traversal ────────────────────────────────────────────────
    println!("── Query 3 of 4: Traversal (who does Alice know?) ──");
    println!("  db.get_outgoing_edges_with_label(alice_id, \"KNOWS\")");
    for edge_id in db.get_outgoing_edges_with_label(alice, "KNOWS") {
        let target = db
            .get_edge_target(edge_id)
            .map_err(|e| format!("failed to resolve KNOWS target: {e}"))?;
        let person = db
            .get_node(target)
            .map_err(|e| format!("failed to load known person: {e}"))?;
        println!(
            "  → Alice KNOWS {} ({})",
            demo_prop_str(&person, "name"),
            demo_prop_str(&person, "title")
        );
    }
    println!("  what you just saw: a single-hop graph traversal over the current graph.\n");

    // ── Query 4: full history / provenance ──────────────────────────────────
    println!("── Query 4 of 4: History — every version of a fact ──");
    println!("  db.get_node_history(alice_id)");
    let history = db
        .get_node_history(alice)
        .map_err(|e| format!("failed to load Alice's history: {e}"))?;
    for version in &history.versions {
        let title = version
            .properties
            .get("title")
            .map(demo_display_value)
            .unwrap_or_else(|| "<none>".to_string());
        println!("  → v{}: title = {title}", version.version_number);
    }
    println!(
        "  what you just saw: {} preserved versions of one node — the audit trail is free.\n",
        history.versions.len()
    );

    println!("────────────────────────────────────────────────────────");
    println!("Next: open a durable database with `AletheiaDB::open(\"./mydb\")`,");
    println!("or point an MCP client at AletheiaDB — see docs/guides/quickstart.md.");

    Ok(())
}

/// Builds the JSON success line emitted by `aletheia restore`.
///
/// The data-directory path is serialized through `serde_json` so any characters
/// that are special in JSON strings (notably Windows path backslashes, e.g.
/// `C:\Users\...`, which are invalid `\U`/`\R` escapes if interpolated raw) are
/// correctly escaped and the output stays valid JSON on every platform.
fn restore_success_json(data_dir: &Path) -> Result<String, String> {
    let value = serde_json::json!({
        "ok": true,
        "data_dir": data_dir.display().to_string(),
    });
    serde_json::to_string(&value).map_err(|e| format!("failed to render JSON output: {e}"))
}

/// Opens the AletheiaDB database, honouring environment-driven config:
/// `ALETHEIADB_CONFIG` (TOML path) takes precedence over `ALETHEIADB_DATA_DIR`
/// (canonical durable layout). With neither set the database is ephemeral.
///
/// Converts underlying database errors into a clean string for CLI output.
fn open_db() -> Result<AletheiaDB, String> {
    AletheiaDB::open_from_env().map_err(|e| format!("failed to initialize database: {e}"))
}

/// Handles all subcommands under `aletheia node`.
///
/// Routes to either `create` or `get` based on the first argument in `args`.
fn handle_node(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("create") => {
            if args.len() < 2 {
                return Err("usage: aletheia node create <label> [--properties JSON]".to_string());
            }
            let label = &args[1];
            let properties = parse_optional_properties(&args[2..])?;
            let db = open_db()?;
            let node_id = db
                .create_node(label, properties)
                .map_err(|e| format!("create_node failed: {e}"))?;
            print_json_pretty(&serde_json::json!({ "node_id": node_id.as_u64() }))
        }
        Some("get") => {
            if args.len() != 2 {
                return Err("usage: aletheia node get <node_id>".to_string());
            }
            let node_id = parse_node_id(&args[1])?;
            let db = open_db()?;
            let node = db
                .get_node(node_id)
                .map_err(|e| format!("get_node failed: {e}"))?;
            print_json_pretty(&node_to_json(&node))
        }
        Some(sub) => Err(format!("unknown node subcommand '{sub}'")),
        None => Err("usage: aletheia node <create|get> ...".to_string()),
    }
}

/// Handles all subcommands under `aletheia edge`.
///
/// Routes to either `create` or `get` based on the first argument in `args`.
fn handle_edge(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("create") => {
            if args.len() < 4 {
                return Err(
                    "usage: aletheia edge create <source_id> <target_id> <label> [--properties JSON]"
                        .to_string(),
                );
            }
            let source = parse_node_id(&args[1])?;
            let target = parse_node_id(&args[2])?;
            let label = &args[3];
            let properties = parse_optional_properties(&args[4..])?;
            let db = open_db()?;
            let edge_id = db
                .create_edge(source, target, label, properties)
                .map_err(|e| format!("create_edge failed: {e}"))?;
            print_json_pretty(&serde_json::json!({ "edge_id": edge_id.as_u64() }))
        }
        Some("get") => {
            if args.len() != 2 {
                return Err("usage: aletheia edge get <edge_id>".to_string());
            }
            let edge_id = parse_edge_id(&args[1])?;
            let db = open_db()?;
            let edge = db
                .get_edge(edge_id)
                .map_err(|e| format!("get_edge failed: {e}"))?;
            print_json_pretty(&edge_to_json(&edge))
        }
        Some(sub) => Err(format!("unknown edge subcommand '{sub}'")),
        None => Err("usage: aletheia edge <create|get> ...".to_string()),
    }
}

/// Handles the `aletheia traverse` command.
///
/// Performs a single-hop graph traversal from a starting node, following edges
/// with a specific label in the specified direction. Outputs results as JSON.
fn handle_traverse(args: Vec<String>) -> Result<(), String> {
    if args.len() < 2 {
        return Err(
            "usage: aletheia traverse <start_node_id> <edge_label> [--direction outgoing|incoming|both]"
                .to_string(),
        );
    }

    let start = parse_node_id(&args[0])?;
    let label = &args[1];
    let direction = parse_direction(&args[2..])?;

    let db = open_db()?;
    let mut reached = Vec::new();

    if direction == "outgoing" || direction == "both" {
        for edge_id in db.get_outgoing_edges_with_label(start, label) {
            let target = db.get_edge_target(edge_id).map_err(|e| {
                format!(
                    "failed to resolve target for edge {}: {e}",
                    edge_id.as_u64()
                )
            })?;
            reached.push(serde_json::json!({
                "edge_id": edge_id.as_u64(),
                "direction": "outgoing",
                "node_id": target.as_u64(),
            }));
        }
    }

    if direction == "incoming" || direction == "both" {
        for edge_id in db.get_incoming_edges_with_label(start, label) {
            let source = db.get_edge_source(edge_id).map_err(|e| {
                format!(
                    "failed to resolve source for edge {}: {e}",
                    edge_id.as_u64()
                )
            })?;
            reached.push(serde_json::json!({
                "edge_id": edge_id.as_u64(),
                "direction": "incoming",
                "node_id": source.as_u64(),
            }));
        }
    }

    print_json_pretty(&serde_json::json!({
        "start_node_id": start.as_u64(),
        "edge_label": label,
        "direction": direction,
        "results": reached,
    }))
}

/// Handles all subcommands under `aletheia daemon`.
///
/// Routes to `start`, `stop`, or `status` to manage the background server process.
fn handle_daemon(args: Vec<String>) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("start") => daemon_start(&args[1..]),
        Some("stop") => daemon_stop(&args[1..]),
        Some("status") => daemon_status(&args[1..]),
        Some(sub) => Err(format!("unknown daemon subcommand '{sub}'")),
        None => Err("usage: aletheia daemon <start|stop|status> ...".to_string()),
    }
}

/// Starts the AletheiaDB background server daemon.
///
/// 1. Checks if it's already running.
/// 2. Spawns the `aletheia-server` process in the background.
/// 3. Writes the PID and executable path to `.aletheia/daemon.pid`.
fn daemon_start(args: &[String]) -> Result<(), String> {
    let pid_file = PathBuf::from(
        arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string()),
    );
    let log_file = PathBuf::from(
        arg_value(args, "--log-file").unwrap_or_else(|| DEFAULT_LOG_FILE.to_string()),
    );
    let host = arg_value(args, "--host").unwrap_or_else(|| DEFAULT_HOST.to_string());
    let port = arg_value(args, "--port")
        .map(|s| {
            s.parse::<u16>()
                .map_err(|e| format!("invalid port '{s}': {e}"))
        })
        .transpose()?
        .unwrap_or(DEFAULT_PORT);

    if let Some(meta) = read_daemon_metadata(&pid_file)?
        && is_expected_daemon_running(&meta)
    {
        return Err(format!(
            "daemon already running with pid {} ({})",
            meta.pid,
            meta.server_exe.display()
        ));
    }

    ensure_parent_dir(&pid_file)?;
    ensure_parent_dir(&log_file)?;

    let server_exe = resolve_server_executable()?;

    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .map_err(|e| format!("failed to open log file '{}': {e}", log_file.display()))?;

    let mut cmd = Command::new(&server_exe);
    cmd.env("ALETHEIADB_HOST", &host)
        .env("ALETHEIADB_PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(|e| e.to_string())?))
        .stderr(Stdio::from(log));

    let child = cmd.spawn().map_err(|e| {
        format!(
            "failed to launch daemon process '{}': {e}",
            server_exe.display()
        )
    })?;

    let metadata = DaemonMetadata {
        pid: child.id(),
        server_exe,
    };

    write_daemon_metadata(&pid_file, &metadata)?;

    println!(
        "daemon started (pid={}, host={}, port={}, exe={}, log={})",
        metadata.pid,
        host,
        port,
        metadata.server_exe.display(),
        log_file.display()
    );
    Ok(())
}

/// Stops the running AletheiaDB background server daemon.
///
/// Reads the PID from the pid-file, verifies the process is still the expected
/// server executable, and sends a kill signal. Cleans up the pid-file afterwards.
fn daemon_stop(args: &[String]) -> Result<(), String> {
    let pid_file = PathBuf::from(
        arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string()),
    );
    let meta = read_daemon_metadata(&pid_file)?.ok_or_else(|| {
        format!(
            "no pid file found at '{}' (daemon not running?)",
            pid_file.display()
        )
    })?;

    if !is_expected_daemon_running(&meta) {
        return Err(format!(
            "refusing to stop pid {}: process does not match expected daemon binary '{}'",
            meta.pid,
            meta.server_exe.display()
        ));
    }

    let status = Command::new("kill")
        .arg(meta.pid.to_string())
        .status()
        .map_err(|e| format!("failed to invoke kill: {e}"))?;

    if !status.success() {
        return Err(format!("failed to stop daemon process {}", meta.pid));
    }

    fs::remove_file(&pid_file)
        .map_err(|e| format!("failed to remove pid file '{}': {e}", pid_file.display()))?;

    println!("daemon stopped (pid={})", meta.pid);
    Ok(())
}

/// Checks and prints the status of the background server daemon.
///
/// Verifies whether the process ID in the pid-file is actively running and
/// matches the expected server executable.
fn daemon_status(args: &[String]) -> Result<(), String> {
    let pid_file = PathBuf::from(
        arg_value(args, "--pid-file").unwrap_or_else(|| DEFAULT_PID_FILE.to_string()),
    );
    match read_daemon_metadata(&pid_file)? {
        Some(meta) if is_expected_daemon_running(&meta) => {
            println!(
                "daemon is running (pid={}, exe={})",
                meta.pid,
                meta.server_exe.display()
            );
            Ok(())
        }
        Some(meta) => {
            println!(
                "daemon is not running or pid was reused (pid={}, expected_exe={})",
                meta.pid,
                meta.server_exe.display()
            );
            Ok(())
        }
        None => {
            println!("daemon is not running (no pid file)");
            Ok(())
        }
    }
}

/// Resolves the absolute path to the `aletheia-server` executable.
///
/// Assumes the server binary is located in the same directory as this CLI binary.
fn resolve_server_executable() -> Result<PathBuf, String> {
    let mut exe_path =
        env::current_exe().map_err(|e| format!("failed to get current executable path: {e}"))?;

    exe_path.pop();
    let server_exe = exe_path.join(SERVER_BIN_NAME);

    if server_exe.is_file() {
        return Ok(server_exe);
    }

    #[cfg(windows)]
    {
        let server_exe_win = exe_path.join(format!("{}.exe", SERVER_BIN_NAME));
        if server_exe_win.is_file() {
            return Ok(server_exe_win);
        }
    }

    Err(format!(
        "could not find '{}' next to CLI binary at '{}'",
        SERVER_BIN_NAME,
        exe_path.display()
    ))
}

/// Checks if the process defined in `meta` is currently running and is the correct binary.
///
/// Uses `/proc/<pid>` on Unix systems to verify the process executable. This prevents
/// accidentally killing unrelated processes if a PID is reused by the OS after a crash.
fn is_expected_daemon_running(meta: &DaemonMetadata) -> bool {
    let proc_dir = PathBuf::from(format!("/proc/{}", meta.pid));
    if !proc_dir.exists() {
        return false;
    }

    let exe_path = proc_dir.join("exe");
    if let Ok(current_exe) = fs::read_link(&exe_path) {
        return current_exe == meta.server_exe;
    }

    let cmdline_path = proc_dir.join("cmdline");
    let cmdline = fs::read(cmdline_path).unwrap_or_default();
    let joined = String::from_utf8_lossy(&cmdline).replace('\0', " ");
    joined.contains(SERVER_BIN_NAME)
}

/// Writes the daemon's PID and executable path to the specified pid-file.
fn write_daemon_metadata(path: &Path, meta: &DaemonMetadata) -> Result<(), String> {
    let content = format!("{}\n{}\n", meta.pid, meta.server_exe.display());
    fs::write(path, content)
        .map_err(|e| format!("failed to write pid file '{}': {e}", path.display()))
}

/// Reads the daemon's PID and executable path from the specified pid-file.
fn read_daemon_metadata(path: &Path) -> Result<Option<DaemonMetadata>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path)
        .map_err(|e| format!("failed reading pid file '{}': {e}", path.display()))?;

    let mut lines = content.lines();
    let pid_line = lines
        .next()
        .ok_or_else(|| format!("pid file '{}' missing pid line", path.display()))?;
    let exe_line = lines
        .next()
        .ok_or_else(|| format!("pid file '{}' missing executable line", path.display()))?;

    let pid = pid_line
        .parse::<u32>()
        .map_err(|e| format!("invalid pid in '{}': {e}", path.display()))?;

    Ok(Some(DaemonMetadata {
        pid,
        server_exe: PathBuf::from(exe_line),
    }))
}

/// Ensures the parent directory for a given file path exists, creating it if necessary.
fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create directory '{}': {e}", parent.display()))?;
    }
    Ok(())
}

/// Extracts the value of a specific command-line flag (e.g., `--port 1963`).
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(token) = iter.next() {
        if token == flag {
            return iter.next().cloned();
        }
    }
    None
}

/// Parses the `--properties` JSON argument if present, converting it to a `PropertyMap`.
fn parse_optional_properties(args: &[String]) -> Result<PropertyMap, String> {
    match arg_value(args, "--properties") {
        Some(json) => json_to_property_map(&json),
        None => Ok(PropertyMap::new()),
    }
}

/// Parses the `--direction` argument for traversals (defaults to "outgoing").
fn parse_direction(args: &[String]) -> Result<String, String> {
    let direction = arg_value(args, "--direction").unwrap_or_else(|| "outgoing".to_string());
    if matches!(direction.as_str(), "outgoing" | "incoming" | "both") {
        Ok(direction)
    } else {
        Err(format!(
            "invalid direction '{direction}', expected outgoing|incoming|both"
        ))
    }
}

/// Parses a string into a `NodeId`.
fn parse_node_id(raw: &str) -> Result<NodeId, String> {
    let id = raw
        .parse::<u64>()
        .map_err(|e| format!("invalid node id '{raw}': {e}"))?;
    NodeId::new(id).map_err(|e| format!("invalid node id: {e}"))
}

/// Parses a string into an `EdgeId`.
fn parse_edge_id(raw: &str) -> Result<EdgeId, String> {
    let id = raw
        .parse::<u64>()
        .map_err(|e| format!("invalid edge id '{raw}': {e}"))?;
    EdgeId::new(id).map_err(|e| format!("invalid edge id: {e}"))
}

/// Converts a raw JSON string into a structured `PropertyMap`.
fn json_to_property_map(raw: &str) -> Result<PropertyMap, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| format!("invalid JSON properties payload: {e}"))?;

    let object = parsed
        .as_object()
        .ok_or_else(|| "properties JSON must be an object".to_string())?;

    let mut map = PropertyMapBuilder::new();
    for (key, value) in object {
        let converted = json_to_property_value(value)?;
        map = map.insert(key, converted);
    }
    Ok(map.build())
}

/// Converts a single JSON value into an internal `PropertyValue`.
fn json_to_property_value(value: &serde_json::Value) -> Result<PropertyValue, String> {
    match value {
        serde_json::Value::Null => Ok(PropertyValue::Null),
        serde_json::Value::Bool(v) => Ok(PropertyValue::Bool(*v)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(PropertyValue::Int(i))
            } else if let Some(f) = n.as_f64() {
                Ok(PropertyValue::Float(f))
            } else {
                Err("unsupported numeric value".to_string())
            }
        }
        serde_json::Value::String(s) => Ok(PropertyValue::string(s)),
        serde_json::Value::Array(arr) => {
            let values = arr
                .iter()
                .map(json_to_property_value)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(PropertyValue::array(values))
        }
        serde_json::Value::Object(_) => {
            Err("nested objects are not supported in properties".to_string())
        }
    }
}

/// Converts an internal `Node` object into a JSON representation for CLI output.
fn node_to_json(node: &Node) -> serde_json::Value {
    serde_json::json!({
        "id": node.id.as_u64(),
        "label": resolve_label(node.label),
        "properties": property_map_to_json(&node.properties),
    })
}

/// Converts an internal `Edge` object into a JSON representation for CLI output.
fn edge_to_json(edge: &Edge) -> serde_json::Value {
    serde_json::json!({
        "id": edge.id.as_u64(),
        "label": resolve_label(edge.label),
        "source": edge.source.as_u64(),
        "target": edge.target.as_u64(),
        "properties": property_map_to_json(&edge.properties),
    })
}

/// Resolves an `InternedString` back into its raw string representation.
fn resolve_label(label: aletheiadb::InternedString) -> String {
    GLOBAL_INTERNER
        .resolve_with(label, |s| s.to_string())
        .unwrap_or_else(|| "<unknown-label>".to_string())
}

/// Converts an internal `PropertyMap` into a JSON Object.
fn property_map_to_json(props: &PropertyMap) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in props.iter() {
        let key_string = GLOBAL_INTERNER
            .resolve_with(*key, |s| s.to_string())
            .unwrap_or_else(|| "<unknown-key>".to_string());
        map.insert(key_string, property_value_to_json(value));
    }
    serde_json::Value::Object(map)
}

/// Converts an internal `PropertyValue` into a standard JSON Value.
fn property_value_to_json(value: &PropertyValue) -> serde_json::Value {
    match value {
        PropertyValue::Null => serde_json::Value::Null,
        PropertyValue::Bool(v) => serde_json::Value::Bool(*v),
        PropertyValue::Int(v) => serde_json::json!(*v),
        PropertyValue::Float(v) => serde_json::json!(*v),
        PropertyValue::String(v) => serde_json::Value::String(v.to_string()),
        PropertyValue::Bytes(v) => serde_json::json!(v.to_vec()),
        PropertyValue::Array(values) => {
            serde_json::Value::Array(values.iter().map(property_value_to_json).collect())
        }
        PropertyValue::Vector(values) => {
            serde_json::Value::Array(values.iter().map(|f| serde_json::json!(*f)).collect())
        }
        PropertyValue::SparseVector(values) => serde_json::json!({
            "indices": values.indices(),
            "values": values.values(),
            "dimensions": values.dimension(),
        }),
    }
}

/// Pretty-prints a JSON value to standard output.
fn print_json_pretty(value: &serde_json::Value) -> Result<(), String> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|e| format!("failed to render JSON output: {e}"))?;

    let mut stdout = io::stdout().lock();
    match writeln!(stdout, "{rendered}") {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(format!("error writing JSON output: {e}")),
    }
}

/// Signed audit export subcommands (Issue #3358).
#[cfg(feature = "audit-export")]
mod audit {
    use super::{arg_value, open_db, parse_edge_id, parse_node_id};
    use aletheiadb::audit::{
        AuditPublicKey, AuditScope, AuditSigningKey, ExportOptions, verify_json_bytes,
    };
    use std::path::Path;

    /// `aletheia audit-keygen <key_file>` — generate a signing key (0600) and
    /// print its public key. The private seed is written to `<key_file>`.
    pub(super) fn handle_keygen(args: Vec<String>) -> Result<(), String> {
        let path = args
            .first()
            .ok_or_else(|| "usage: aletheia audit-keygen <key_file>".to_string())?;
        if Path::new(path).exists() {
            return Err(format!("refusing to overwrite existing key file '{path}'"));
        }
        let key = AuditSigningKey::generate();
        key.write_to_file(path)
            .map_err(|e| format!("failed to write key file: {e}"))?;
        let value = serde_json::json!({
            "ok": true,
            "key_file": path,
            "public_key": key.public_key().to_hex(),
        });
        let rendered = serde_json::to_string(&value)
            .map_err(|e| format!("failed to render JSON output: {e}"))?;
        println!("{rendered}");
        Ok(())
    }

    /// `aletheia audit-export <node|edge> <id> --key <file> --out <path>` —
    /// sign an entity's full bi-temporal history into a portable artifact.
    pub(super) fn handle_export(args: Vec<String>) -> Result<(), String> {
        let kind = args.first().map(String::as_str).ok_or_else(usage_export)?;
        let id_str = args.get(1).ok_or_else(usage_export)?;
        let key_file = arg_value(&args, "--key").ok_or_else(usage_export)?;
        let out = arg_value(&args, "--out").ok_or_else(usage_export)?;
        let db_id = arg_value(&args, "--db-id").unwrap_or_else(|| "aletheiadb".to_string());

        let scope = match kind {
            "node" => AuditScope::node(parse_node_id(id_str)?),
            "edge" => AuditScope::edge(parse_edge_id(id_str)?),
            other => {
                return Err(format!(
                    "unknown entity kind '{other}' (expected node|edge)"
                ));
            }
        };

        let mut options = ExportOptions::new(db_id);
        if let Some(redact) = arg_value(&args, "--redact") {
            let keys: Vec<String> = redact
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            options = options.redact(keys);
        }

        let key = AuditSigningKey::from_file(&key_file)
            .map_err(|e| format!("failed to load signing key: {e}"))?;
        let db = open_db()?;
        let export = db
            .audit_export(scope, &key, &options)
            .map_err(|e| format!("audit export failed: {e}"))?;
        let bytes = export
            .to_json_bytes()
            .map_err(|e| format!("failed to serialize artifact: {e}"))?;
        std::fs::write(&out, &bytes).map_err(|e| format!("failed to write artifact: {e}"))?;

        let value = serde_json::json!({
            "ok": true,
            "artifact": out,
            "entity_count": export.entity_count(),
            "version_count": export.version_count(),
            "public_key": key.public_key().to_hex(),
            "chain_root": export.chain.root,
            "anchor_lsn": export.metadata().chain_anchor.source_lsn,
        });
        let rendered = serde_json::to_string(&value)
            .map_err(|e| format!("failed to render JSON output: {e}"))?;
        println!("{rendered}");
        Ok(())
    }

    /// `aletheia audit-verify <artifact> [--public-key HEX]` — verify OFFLINE.
    /// Exits non-zero (via `Err`) when verification fails.
    pub(super) fn handle_verify(args: Vec<String>) -> Result<(), String> {
        let path = args.first().ok_or_else(|| {
            "usage: aletheia audit-verify <artifact> [--public-key HEX]".to_string()
        })?;
        let bytes = std::fs::read(path).map_err(|e| format!("failed to read artifact: {e}"))?;

        let expected = match arg_value(&args, "--public-key") {
            Some(hex) => Some(
                AuditPublicKey::from_hex(&hex).map_err(|e| format!("invalid --public-key: {e}"))?,
            ),
            None => None,
        };

        match verify_json_bytes(&bytes, expected.as_ref()) {
            Ok(report) => {
                println!("{}", report.summary());
                println!("  entity summary: {}", report.entity_summary);
                if !report.redacted_keys.is_empty() {
                    println!("  redacted keys: {}", report.redacted_keys.join(", "));
                }
                if !report.trusted_key {
                    println!(
                        "  NOTE: no --public-key supplied; trust in the embedded key is \
                         self-asserted. Supply the signer's known public key to establish trust."
                    );
                }
                Ok(())
            }
            Err(e) => Err(format!("VERIFICATION FAILED: {e}")),
        }
    }

    /// `aletheia audit-render <artifact>` — render a human-readable chronology.
    pub(super) fn handle_render(args: Vec<String>) -> Result<(), String> {
        let path = args
            .first()
            .ok_or_else(|| "usage: aletheia audit-render <artifact>".to_string())?;
        let bytes = std::fs::read(path).map_err(|e| format!("failed to read artifact: {e}"))?;
        let export = aletheiadb::audit::AuditExport::from_json_bytes(&bytes)
            .map_err(|e| format!("failed to parse artifact: {e}"))?;
        print!("{}", export.render_chronology());
        Ok(())
    }

    fn usage_export() -> String {
        "usage: aletheia audit-export <node|edge> <id> --key <key_file> --out <path> \
         [--db-id ID] [--redact k1,k2]"
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_demo_seeds_and_runs_all_showcase_queries() {
        let db = AletheiaDB::new().expect("ephemeral demo database should open");
        // The guided sequence must complete without error against the seed.
        run_demo(&db).expect("run_demo should succeed end-to-end");

        // Seeding must have produced a realistic graph, and the temporal
        // narrative must hold: Alice's latest title differs from her founding
        // title (this is the AS OF differentiator the demo showcases).
        assert!(db.node_count() >= 4, "expected the core cast plus filler");
        assert!(
            db.edge_count() >= 4,
            "expected WORKS_AT/KNOWS relationships"
        );
    }

    #[test]
    fn handle_demo_runs_to_completion() {
        // The one-shot demo needs no arguments and must exit Ok.
        handle_demo(vec![]).expect("aletheia demo should run to completion");
    }

    #[test]
    fn demo_props_builds_expected_map() {
        let map = demo_props(&[("name", "Alice"), ("title", "Engineer")]);
        // Round-trip the inserted values, not just non-emptiness.
        assert_eq!(
            map.get("name").map(demo_display_value).as_deref(),
            Some("Alice"),
            "name should round-trip"
        );
        assert_eq!(
            map.get("title").map(demo_display_value).as_deref(),
            Some("Engineer"),
            "title should round-trip"
        );
    }

    #[test]
    fn handle_backup_missing_arg_returns_usage_error() {
        let err = handle_backup(vec![]).unwrap_err();
        assert!(err.contains("usage:"), "unexpected error: {err}");
    }

    #[test]
    fn handle_restore_missing_arg_returns_usage_error() {
        let err = handle_restore(vec![]).unwrap_err();
        assert!(err.contains("usage:"), "unexpected error: {err}");
    }

    #[test]
    fn handle_restore_missing_data_dir_env_returns_error() {
        // ALETHEIADB_DATA_DIR is not set in the test environment.
        // If it somehow is set, this test exercises a different (later) error path,
        // but it still returns Err, so the assertion holds.
        let result = handle_restore(vec!["nonexistent.albk".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn restore_success_json_escapes_backslash_paths() {
        // kills: reverting handle_restore to raw string interpolation of the data
        // dir path, which emits invalid JSON for Windows backslash paths (`\U`/`\R`
        // are illegal JSON escapes) — the exact failure CI caught on windows-latest.
        // pins: restore output is serde-serialized so it parses AND the data_dir
        // string round-trips byte-for-byte on every platform.
        let win_path = Path::new(r"C:\Users\RUNNER~1\Temp\.tmpX");
        let out = restore_success_json(win_path).expect("serialization must succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("restore output must be valid JSON");
        assert_eq!(parsed["ok"], serde_json::json!(true));
        assert_eq!(
            parsed["data_dir"],
            serde_json::json!(win_path.display().to_string()),
            "data_dir must round-trip exactly through JSON escaping"
        );
    }

    // ========================================================================
    // Issue #3351 — `aletheia verify` helper unit tests.
    // ========================================================================

    fn sample_verification(passed: bool) -> ChainVerification {
        ChainVerification {
            passed,
            head_seq: 7,
            head_digest_hex: "abcd".to_string(),
            earliest_broken_seq: if passed { None } else { Some(3) },
            reason: if passed {
                None
            } else {
                Some("recomputed chain digest differs from sealed digest".to_string())
            },
            transactions_checked: 7,
        }
    }

    #[test]
    fn parse_entity_arg_accepts_node_and_edge() {
        let (kind, id) = parse_entity_arg("node:42").unwrap();
        assert!(matches!(kind, EntityKind::Node));
        assert_eq!(id, 42);
        let (kind, id) = parse_entity_arg("EDGE:7").unwrap();
        assert!(matches!(kind, EntityKind::Edge));
        assert_eq!(id, 7);
    }

    #[test]
    fn parse_entity_arg_rejects_missing_colon() {
        let err = parse_entity_arg("node42").unwrap_err();
        assert!(err.contains("expected"), "unexpected error: {err}");
    }

    #[test]
    fn parse_entity_arg_rejects_unknown_kind() {
        let err = parse_entity_arg("vertex:1").unwrap_err();
        assert!(err.contains("expected 'node' or 'edge'"), "got: {err}");
    }

    #[test]
    fn parse_entity_arg_rejects_non_numeric_id() {
        let err = parse_entity_arg("node:abc").unwrap_err();
        assert!(err.contains("invalid --entity id"), "got: {err}");
    }

    #[test]
    fn render_verification_human_pass_reports_status_and_head() {
        let out = render_verification(&sample_verification(true), "full", false).unwrap();
        assert!(out.contains("PASS"), "got: {out}");
        assert!(out.contains("scope: full"), "got: {out}");
        assert!(out.contains("head seq:             7"), "got: {out}");
        // A clean pass must NOT print a broken seq / reason line.
        assert!(!out.contains("earliest broken seq"), "got: {out}");
    }

    #[test]
    fn render_verification_human_fail_reports_broken_seq_and_reason() {
        let out = render_verification(&sample_verification(false), "entity", false).unwrap();
        assert!(out.contains("FAIL"), "got: {out}");
        assert!(out.contains("earliest broken seq:  3"), "got: {out}");
        assert!(out.contains("differs from sealed digest"), "got: {out}");
    }

    #[test]
    fn render_verification_json_is_machine_readable() {
        let out = render_verification(&sample_verification(false), "anchor", true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(value["scope"], serde_json::json!("anchor"));
        assert_eq!(value["passed"], serde_json::json!(false));
        assert_eq!(value["earliest_broken_seq"], serde_json::json!(3));
        assert_eq!(value["head_seq"], serde_json::json!(7));
    }

    #[test]
    fn finish_verification_returns_err_on_failure() {
        // Failure must map to a non-zero exit (Err), success to Ok.
        assert!(finish_verification(&sample_verification(false), "full", true).is_err());
        assert!(finish_verification(&sample_verification(true), "full", true).is_ok());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn chain_head_round_trips_through_file() {
        let head = ChainHead::genesis(5, 1234);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("head.json");
        let path_str = path.to_str().unwrap();
        write_chain_head(&head, path_str).unwrap();
        let loaded = read_chain_head(path_str).unwrap();
        assert_eq!(head, loaded);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn read_chain_head_rejects_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        fs::write(&path, b"not a chain head").unwrap();
        let err = read_chain_head(path.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("not a valid exported chain head"),
            "got: {err}"
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn render_head_export_human_and_json() {
        let head = ChainHead::genesis(9, 42);
        let human = render_head_export(&head, "/tmp/x.json", false).unwrap();
        assert!(human.contains("Exported chain head anchor to /tmp/x.json"));
        assert!(human.contains("seq:    0"));
        let json = render_head_export(&head, "/tmp/x.json", true).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["ok"], serde_json::json!(true));
        assert_eq!(value["seq"], serde_json::json!(0));
    }

    #[test]
    fn parse_direction_defaults_to_outgoing() {
        let dir = parse_direction(&[]).unwrap();
        assert_eq!(dir, "outgoing");
    }

    #[test]
    fn parse_direction_accepts_incoming() {
        let dir = parse_direction(&["--direction".to_string(), "incoming".to_string()]).unwrap();
        assert_eq!(dir, "incoming");
    }

    #[test]
    fn parse_direction_accepts_both() {
        let dir = parse_direction(&["--direction".to_string(), "both".to_string()]).unwrap();
        assert_eq!(dir, "both");
    }

    #[test]
    fn parse_direction_rejects_invalid() {
        let err =
            parse_direction(&["--direction".to_string(), "sideways".to_string()]).unwrap_err();
        assert!(err.contains("invalid direction"), "unexpected error: {err}");
    }

    #[test]
    fn parse_node_id_rejects_non_numeric() {
        let err = parse_node_id("abc").unwrap_err();
        assert!(err.contains("invalid node id"), "unexpected error: {err}");
    }

    #[test]
    fn parse_edge_id_rejects_non_numeric() {
        let err = parse_edge_id("xyz").unwrap_err();
        assert!(err.contains("invalid edge id"), "unexpected error: {err}");
    }

    #[test]
    fn json_to_property_map_parses_mixed_types() {
        let map = json_to_property_map(r#"{"name":"Alice","age":30,"active":true}"#).unwrap();
        assert!(!map.is_empty());
    }

    #[test]
    fn json_to_property_map_rejects_non_object() {
        let err = json_to_property_map(r#"[1,2,3]"#).unwrap_err();
        assert!(err.contains("must be an object"), "unexpected error: {err}");
    }

    #[test]
    fn json_to_property_map_rejects_invalid_json() {
        let err = json_to_property_map("not json").unwrap_err();
        assert!(err.contains("invalid JSON"), "unexpected error: {err}");
    }

    #[test]
    fn arg_value_finds_flag() {
        let args = vec!["--port".to_string(), "1234".to_string()];
        assert_eq!(arg_value(&args, "--port"), Some("1234".to_string()));
    }

    #[test]
    fn arg_value_returns_none_when_absent() {
        let args = vec!["--host".to_string(), "localhost".to_string()];
        assert_eq!(arg_value(&args, "--port"), None);
    }

    // ========================================================================
    // Issue #3480 — mutation-kill unit tests (Layer 1).
    //
    // These cover pure-helper RETURN VALUES and BOUNDARIES not exercised by the
    // 16 tests above, so return-value-stub and condition-flip mutants are killed.
    // ========================================================================

    // --- parse_direction: explicit "outgoing" branch (default is separately tested) ---

    #[test]
    fn parse_direction_accepts_explicit_outgoing() {
        // kills: match-arm / return-value stubs collapsing explicit "outgoing"
        // into the error path (the default path already returns "outgoing").
        let dir = parse_direction(&["--direction".to_string(), "outgoing".to_string()]).unwrap();
        assert_eq!(dir, "outgoing");
    }

    // --- parse_node_id / parse_edge_id: happy paths + overflow boundary ---

    #[test]
    fn parse_node_id_accepts_valid_numeric() {
        // kills: return-value stubs that ignore the parsed id.
        let id = parse_node_id("42").unwrap();
        assert_eq!(id.as_u64(), 42);
    }

    #[test]
    fn parse_node_id_accepts_zero() {
        // kills: off-by-one / boundary flips rejecting the lowest valid id.
        let id = parse_node_id("0").unwrap();
        assert_eq!(id.as_u64(), 0);
    }

    #[test]
    fn parse_node_id_rejects_out_of_range() {
        // kills: flips that skip MAX_VALID_ID validation (u64::MAX - 1000 guard).
        let err = parse_node_id(&u64::MAX.to_string()).unwrap_err();
        assert!(err.contains("invalid node id"), "unexpected error: {err}");
    }

    #[test]
    fn parse_edge_id_accepts_valid_numeric() {
        // kills: return-value stubs that ignore the parsed id.
        let id = parse_edge_id("7").unwrap();
        assert_eq!(id.as_u64(), 7);
    }

    #[test]
    fn parse_edge_id_rejects_out_of_range() {
        // kills: flips that skip EdgeId::new range validation.
        let err = parse_edge_id(&u64::MAX.to_string()).unwrap_err();
        assert!(err.contains("invalid edge id"), "unexpected error: {err}");
    }

    // --- json_to_property_value: exact variant per JSON kind + nested-object rejection ---

    #[test]
    fn json_to_property_value_null() {
        // kills: match-arm swaps mapping Null to a different variant.
        let v = json_to_property_value(&serde_json::Value::Null).unwrap();
        assert!(matches!(v, PropertyValue::Null), "got {v:?}");
    }

    #[test]
    fn json_to_property_value_bool_true() {
        // kills: `*v` -> literal-true/false stubs on the Bool arm.
        let v = json_to_property_value(&serde_json::json!(true)).unwrap();
        assert!(matches!(v, PropertyValue::Bool(true)), "got {v:?}");
    }

    #[test]
    fn json_to_property_value_bool_false() {
        // kills: Bool arm stubbed to a constant true.
        let v = json_to_property_value(&serde_json::json!(false)).unwrap();
        assert!(matches!(v, PropertyValue::Bool(false)), "got {v:?}");
    }

    #[test]
    fn json_to_property_value_integer_maps_to_int() {
        // kills: swapping the Int/Float branch order or ignoring the value.
        let v = json_to_property_value(&serde_json::json!(30)).unwrap();
        assert!(matches!(v, PropertyValue::Int(30)), "got {v:?}");
    }

    #[test]
    fn json_to_property_value_negative_integer_maps_to_int() {
        // kills: as_i64/as_f64 ordering flips (negatives must stay Int).
        let v = json_to_property_value(&serde_json::json!(-5)).unwrap();
        assert!(matches!(v, PropertyValue::Int(-5)), "got {v:?}");
    }

    #[test]
    fn json_to_property_value_float_maps_to_float() {
        // kills: dropping the as_f64 fallback branch.
        let v = json_to_property_value(&serde_json::json!(1.5)).unwrap();
        match v {
            PropertyValue::Float(f) => assert!((f - 1.5).abs() < f64::EPSILON, "got {f}"),
            other => panic!("expected Float, got {other:?}"),
        }
    }

    #[test]
    fn json_to_property_value_string() {
        // kills: String arm stubbed to empty/constant.
        let v = json_to_property_value(&serde_json::json!("hello")).unwrap();
        match v {
            PropertyValue::String(ref s) => assert_eq!(s.to_string(), "hello"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn json_to_property_value_array_preserves_elements() {
        // kills: Array arm stubbed to empty, or element conversion dropped.
        let v = json_to_property_value(&serde_json::json!([1, "two", true])).unwrap();
        match v {
            PropertyValue::Array(ref items) => {
                assert_eq!(items.len(), 3, "array length must be preserved");
                assert!(
                    matches!(items[0], PropertyValue::Int(1)),
                    "got {:?}",
                    items[0]
                );
                assert!(
                    matches!(&items[1], PropertyValue::String(s) if s.to_string() == "two"),
                    "got {:?}",
                    items[1]
                );
                assert!(
                    matches!(items[2], PropertyValue::Bool(true)),
                    "got {:?}",
                    items[2]
                );
            }
            other => panic!("expected Array, got {other:?}"),
        }
    }

    #[test]
    fn json_to_property_value_rejects_nested_object() {
        // kills: removing the Object rejection arm (would silently accept/drop
        // nested objects instead of erroring).
        let err = json_to_property_value(&serde_json::json!({"a": 1})).unwrap_err();
        assert!(
            err.contains("nested objects are not supported"),
            "unexpected error: {err}"
        );
    }

    // --- json_to_property_map: value fidelity via round-trip ---

    #[test]
    #[serial_test::serial]
    fn json_to_property_map_roundtrips_values() {
        // kills: key/value drops in json_to_property_map (interns keys, so serial).
        let map = json_to_property_map(r#"{"name":"Alice","age":30,"active":true}"#).unwrap();
        let json = property_map_to_json(&map);
        let obj = json.as_object().expect("expected JSON object");
        assert_eq!(obj.get("name"), Some(&serde_json::json!("Alice")));
        assert_eq!(obj.get("age"), Some(&serde_json::json!(30)));
        assert_eq!(obj.get("active"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn json_to_property_map_rejects_nested_object_value() {
        // kills: dropping the propagated nested-object rejection from a map value.
        let err = json_to_property_map(r#"{"outer":{"inner":1}}"#).unwrap_err();
        assert!(
            err.contains("nested objects are not supported"),
            "unexpected error: {err}"
        );
    }

    // --- property_value_to_json: scalar + Vector + SparseVector shapes ---

    #[test]
    fn property_value_to_json_scalars() {
        // kills: match-arm swaps on the scalar branches.
        assert_eq!(
            property_value_to_json(&PropertyValue::Null),
            serde_json::Value::Null
        );
        assert_eq!(
            property_value_to_json(&PropertyValue::Bool(true)),
            serde_json::json!(true)
        );
        assert_eq!(
            property_value_to_json(&PropertyValue::Int(-9)),
            serde_json::json!(-9)
        );
        assert_eq!(
            property_value_to_json(&PropertyValue::string("x")),
            serde_json::json!("x")
        );
        match property_value_to_json(&PropertyValue::Float(2.5)) {
            serde_json::Value::Number(n) => {
                assert!((n.as_f64().unwrap() - 2.5).abs() < f64::EPSILON)
            }
            other => panic!("expected number, got {other:?}"),
        }
        assert_eq!(
            property_value_to_json(&PropertyValue::bytes([1u8, 2, 3])),
            serde_json::json!([1, 2, 3])
        );
        assert_eq!(
            property_value_to_json(&PropertyValue::array(vec![
                PropertyValue::Int(1),
                PropertyValue::Int(2),
            ])),
            serde_json::json!([1, 2])
        );
    }

    #[test]
    fn property_value_to_json_dense_vector_is_flat_array() {
        // kills: Vector arm stubbed to null/empty; must render a flat float array.
        let json = property_value_to_json(&PropertyValue::vector([0.5f32, 1.5, 2.5]));
        let arr = json.as_array().expect("vector must render as JSON array");
        assert_eq!(arr.len(), 3);
        assert!((arr[0].as_f64().unwrap() - 0.5).abs() < 1e-6);
        assert!((arr[2].as_f64().unwrap() - 2.5).abs() < 1e-6);
    }

    #[test]
    fn property_value_to_json_sparse_vector_shape() {
        // kills: SparseVector arm field drops (indices/values/dimensions).
        use aletheiadb::core::vector::SparseVec;
        let sparse = SparseVec::new(vec![1u32, 4], vec![0.5f32, -0.25], 8).unwrap();
        let json = property_value_to_json(&PropertyValue::sparse_vector(sparse));
        let obj = json
            .as_object()
            .expect("sparse vector must render as object");
        assert_eq!(obj.get("indices"), Some(&serde_json::json!([1, 4])));
        assert_eq!(obj.get("dimensions"), Some(&serde_json::json!(8)));
        let values = obj
            .get("values")
            .and_then(|v| v.as_array())
            .expect("values array");
        assert_eq!(values.len(), 2);
        assert!((values[0].as_f64().unwrap() - 0.5).abs() < 1e-6);
    }

    // --- property_map_to_json: key/value round-trip ---

    #[test]
    #[serial_test::serial]
    fn property_map_to_json_roundtrips_keys_and_values() {
        // kills: key resolution / value drops (touches GLOBAL_INTERNER, so serial).
        let map = PropertyMapBuilder::new()
            .insert("k1", "v1")
            .insert("k2", 7i64)
            .build();
        let json = property_map_to_json(&map);
        let obj = json.as_object().expect("expected JSON object");
        assert_eq!(obj.len(), 2);
        assert_eq!(obj.get("k1"), Some(&serde_json::json!("v1")));
        assert_eq!(obj.get("k2"), Some(&serde_json::json!(7)));
    }

    // --- node_to_json / edge_to_json: field fidelity (kills field-drop stubs) ---

    #[test]
    #[serial_test::serial]
    fn node_to_json_contains_id_label_and_properties() {
        // kills: dropping/mislabeling the id, label, or properties fields.
        let db = AletheiaDB::new().unwrap();
        let id = db
            .create_node(
                "Person",
                PropertyMapBuilder::new().insert("name", "Alice").build(),
            )
            .unwrap();
        let node = db.get_node(id).unwrap();
        let json = node_to_json(&node);
        assert_eq!(json.get("id"), Some(&serde_json::json!(id.as_u64())));
        assert_eq!(json.get("label"), Some(&serde_json::json!("Person")));
        assert_eq!(
            json.get("properties").and_then(|p| p.get("name")),
            Some(&serde_json::json!("Alice"))
        );
    }

    #[test]
    #[serial_test::serial]
    fn edge_to_json_contains_endpoints_label_and_properties() {
        // kills: swapping/dropping source, target, id, label, or properties.
        let db = AletheiaDB::new().unwrap();
        let src = db.create_node("Person", PropertyMap::new()).unwrap();
        let dst = db.create_node("Person", PropertyMap::new()).unwrap();
        let edge_id = db
            .create_edge(
                src,
                dst,
                "KNOWS",
                PropertyMapBuilder::new().insert("since", 2020i64).build(),
            )
            .unwrap();
        let edge = db.get_edge(edge_id).unwrap();
        let json = edge_to_json(&edge);
        assert_eq!(json.get("id"), Some(&serde_json::json!(edge_id.as_u64())));
        assert_eq!(json.get("label"), Some(&serde_json::json!("KNOWS")));
        assert_eq!(json.get("source"), Some(&serde_json::json!(src.as_u64())));
        assert_eq!(json.get("target"), Some(&serde_json::json!(dst.as_u64())));
        assert_eq!(
            json.get("properties").and_then(|p| p.get("since")),
            Some(&serde_json::json!(2020))
        );
    }

    // --- resolve_label: resolves a known interned label back to its string ---

    #[test]
    #[serial_test::serial]
    fn resolve_label_returns_interned_string() {
        // kills: resolve_label stubbed to the "<unknown-label>" fallback.
        let db = AletheiaDB::new().unwrap();
        let id = db.create_node("Widget", PropertyMap::new()).unwrap();
        let node = db.get_node(id).unwrap();
        assert_eq!(resolve_label(node.label), "Widget");
    }

    // --- parse_optional_properties: empty vs populated ---

    #[test]
    fn parse_optional_properties_defaults_empty() {
        // kills: returning a non-empty map when no --properties is supplied.
        let map = parse_optional_properties(&[]).unwrap();
        assert!(map.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn parse_optional_properties_parses_supplied_json() {
        // kills: ignoring the --properties value (interns keys, so serial).
        let map = parse_optional_properties(&[
            "--properties".to_string(),
            r#"{"name":"Bob"}"#.to_string(),
        ])
        .unwrap();
        assert!(!map.is_empty());
        let json = property_map_to_json(&map);
        assert_eq!(json.get("name"), Some(&serde_json::json!("Bob")));
    }

    #[test]
    fn parse_optional_properties_propagates_parse_error() {
        // kills: swallowing a malformed --properties payload.
        let err = parse_optional_properties(&["--properties".to_string(), "not json".to_string()])
            .unwrap_err();
        assert!(err.contains("invalid JSON"), "unexpected error: {err}");
    }
}
