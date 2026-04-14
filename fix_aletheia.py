import re

with open("src/bin/aletheia.rs", "r") as f:
    content = f.read()

def replace_or_fail(search, replace):
    global content
    if search not in content:
        raise ValueError(f"Search string not found:\n{search}")
    content = content.replace(search, replace)

replace_or_fail('''fn main() {''', '''/// The main entry point for the AletheiaDB CLI.
///
/// This simply wraps `run()` and handles exiting with the correct status code
/// if an error occurs.
fn main() {''')

replace_or_fail('''fn run() -> Result<(), String> {''', '''/// Parses CLI arguments and routes to the appropriate subcommand handler.
///
/// # Returns
/// - `Ok(())` if the command succeeded.
/// - `Err(String)` with an error message if something failed or syntax was wrong.
fn run() -> Result<(), String> {''')

replace_or_fail('''fn print_usage() {''', '''/// Prints the CLI usage instructions to standard output.
fn print_usage() {''')

replace_or_fail('''fn open_db() -> Result<AletheiaDB, String> {''', '''/// Opens the AletheiaDB database using the default configuration.
///
/// Converts underlying database errors into a clean string for CLI output.
fn open_db() -> Result<AletheiaDB, String> {''')

replace_or_fail('''fn handle_node(args: Vec<String>) -> Result<(), String> {''', '''/// Handles all subcommands under `aletheia node`.
///
/// Routes to either `create` or `get` based on the first argument in `args`.
fn handle_node(args: Vec<String>) -> Result<(), String> {''')

replace_or_fail('''fn handle_edge(args: Vec<String>) -> Result<(), String> {''', '''/// Handles all subcommands under `aletheia edge`.
///
/// Routes to either `create` or `get` based on the first argument in `args`.
fn handle_edge(args: Vec<String>) -> Result<(), String> {''')

replace_or_fail('''fn handle_traverse(args: Vec<String>) -> Result<(), String> {''', '''/// Handles the `aletheia traverse` command.
///
/// Performs a single-hop graph traversal from a starting node, following edges
/// with a specific label in the specified direction. Outputs results as JSON.
fn handle_traverse(args: Vec<String>) -> Result<(), String> {''')

replace_or_fail('''fn handle_daemon(args: Vec<String>) -> Result<(), String> {''', '''/// Handles all subcommands under `aletheia daemon`.
///
/// Routes to `start`, `stop`, or `status` to manage the background server process.
fn handle_daemon(args: Vec<String>) -> Result<(), String> {''')

replace_or_fail('''fn daemon_start(args: &[String]) -> Result<(), String> {''', '''/// Starts the AletheiaDB background server daemon.
///
/// 1. Checks if it's already running.
/// 2. Spawns the `aletheia-server` process in the background.
/// 3. Writes the PID and executable path to `.aletheia/daemon.pid`.
fn daemon_start(args: &[String]) -> Result<(), String> {''')

replace_or_fail('''fn daemon_stop(args: &[String]) -> Result<(), String> {''', '''/// Stops the running AletheiaDB background server daemon.
///
/// Reads the PID from the pid-file, verifies the process is still the expected
/// server executable, and sends a kill signal. Cleans up the pid-file afterwards.
fn daemon_stop(args: &[String]) -> Result<(), String> {''')

replace_or_fail('''fn daemon_status(args: &[String]) -> Result<(), String> {''', '''/// Checks and prints the status of the background server daemon.
///
/// Verifies whether the process ID in the pid-file is actively running and
/// matches the expected server executable.
fn daemon_status(args: &[String]) -> Result<(), String> {''')

replace_or_fail('''fn resolve_server_executable() -> Result<PathBuf, String> {''', '''/// Resolves the absolute path to the `aletheia-server` executable.
///
/// Assumes the server binary is located in the same directory as this CLI binary.
fn resolve_server_executable() -> Result<PathBuf, String> {''')

replace_or_fail('''fn is_expected_daemon_running(meta: &DaemonMetadata) -> bool {''', '''/// Checks if the process defined in `meta` is currently running and is the correct binary.
///
/// Uses `/proc/<pid>` on Unix systems to verify the process executable. This prevents
/// accidentally killing unrelated processes if a PID is reused by the OS after a crash.
fn is_expected_daemon_running(meta: &DaemonMetadata) -> bool {''')

replace_or_fail('''fn write_daemon_metadata(path: &Path, meta: &DaemonMetadata) -> Result<(), String> {''', '''/// Writes the daemon's PID and executable path to the specified pid-file.
fn write_daemon_metadata(path: &Path, meta: &DaemonMetadata) -> Result<(), String> {''')

replace_or_fail('''fn read_daemon_metadata(path: &Path) -> Result<Option<DaemonMetadata>, String> {''', '''/// Reads the daemon's PID and executable path from the specified pid-file.
fn read_daemon_metadata(path: &Path) -> Result<Option<DaemonMetadata>, String> {''')

replace_or_fail('''fn ensure_parent_dir(path: &Path) -> Result<(), String> {''', '''/// Ensures the parent directory for a given file path exists, creating it if necessary.
fn ensure_parent_dir(path: &Path) -> Result<(), String> {''')

replace_or_fail('''fn arg_value(args: &[String], flag: &str) -> Option<String> {''', '''/// Extracts the value of a specific command-line flag (e.g., `--port 8080`).
fn arg_value(args: &[String], flag: &str) -> Option<String> {''')

replace_or_fail('''fn parse_optional_properties(args: &[String]) -> Result<PropertyMap, String> {''', '''/// Parses the `--properties` JSON argument if present, converting it to a `PropertyMap`.
fn parse_optional_properties(args: &[String]) -> Result<PropertyMap, String> {''')

replace_or_fail('''fn parse_direction(args: &[String]) -> Result<String, String> {''', '''/// Parses the `--direction` argument for traversals (defaults to "outgoing").
fn parse_direction(args: &[String]) -> Result<String, String> {''')

replace_or_fail('''fn parse_node_id(raw: &str) -> Result<NodeId, String> {''', '''/// Parses a string into a `NodeId`.
fn parse_node_id(raw: &str) -> Result<NodeId, String> {''')

replace_or_fail('''fn parse_edge_id(raw: &str) -> Result<EdgeId, String> {''', '''/// Parses a string into an `EdgeId`.
fn parse_edge_id(raw: &str) -> Result<EdgeId, String> {''')

replace_or_fail('''fn json_to_property_map(raw: &str) -> Result<PropertyMap, String> {''', '''/// Converts a raw JSON string into a structured `PropertyMap`.
fn json_to_property_map(raw: &str) -> Result<PropertyMap, String> {''')

replace_or_fail('''fn json_to_property_value(value: &serde_json::Value) -> Result<PropertyValue, String> {''', '''/// Converts a single JSON value into an internal `PropertyValue`.
fn json_to_property_value(value: &serde_json::Value) -> Result<PropertyValue, String> {''')

replace_or_fail('''fn node_to_json(node: &Node) -> serde_json::Value {''', '''/// Converts an internal `Node` object into a JSON representation for CLI output.
fn node_to_json(node: &Node) -> serde_json::Value {''')

replace_or_fail('''fn edge_to_json(edge: &Edge) -> serde_json::Value {''', '''/// Converts an internal `Edge` object into a JSON representation for CLI output.
fn edge_to_json(edge: &Edge) -> serde_json::Value {''')

replace_or_fail('''fn resolve_label(label: aletheiadb::InternedString) -> String {''', '''/// Resolves an `InternedString` back into its raw string representation.
fn resolve_label(label: aletheiadb::InternedString) -> String {''')

replace_or_fail('''fn property_map_to_json(props: &PropertyMap) -> serde_json::Value {''', '''/// Converts an internal `PropertyMap` into a JSON Object.
fn property_map_to_json(props: &PropertyMap) -> serde_json::Value {''')

replace_or_fail('''fn property_value_to_json(value: &PropertyValue) -> serde_json::Value {''', '''/// Converts an internal `PropertyValue` into a standard JSON Value.
fn property_value_to_json(value: &PropertyValue) -> serde_json::Value {''')

replace_or_fail('''fn print_json_pretty(value: &serde_json::Value) -> Result<(), String> {''', '''/// Pretty-prints a JSON value to standard output.
fn print_json_pretty(value: &serde_json::Value) -> Result<(), String> {''')


with open("src/bin/aletheia.rs", "w") as f:
    f.write(content)
