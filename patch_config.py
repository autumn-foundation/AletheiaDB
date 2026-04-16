with open("src/config.rs", "r") as f:
    content = f.read()

target = """pub struct WalConfig {"""
replacement = """/// Configuration for WAL (Write-Ahead Log) system.
///
/// # The Context
/// The WAL is responsible for ensuring durability in AletheiaDB. This struct
/// configures the size limits, flush intervals, and sync behaviors of the WAL
/// to tune the trade-off between performance and durability.
pub struct WalConfig {"""
content = content.replace(target, replacement)

target = """pub struct HistoricalConfig {"""
replacement = """/// Configuration for Historical Storage subsystem.
///
/// # The Context
/// This controls how AletheiaDB manages old versions of graph entities. It limits
/// how many versions of a node or edge are retained, and the depth of reconstruction
/// permitted during temporal queries.
pub struct HistoricalConfig {"""
content = content.replace(target, replacement)

target = """pub struct VectorIndexConfig {"""
replacement = """/// Configuration for Vector Index subsystem.
///
/// # The Context
/// This determines the resource allocation and performance tuning parameters
/// for HNSW-based vector indexes, such as memory capacity, cache limits,
/// and SIMD acceleration paths.
pub struct VectorIndexConfig {"""
content = content.replace(target, replacement)

target = """pub struct AletheiaDBConfig {"""
replacement = """/// Unified Database Configuration.
///
/// # The Context
/// This is the top-level configuration object for AletheiaDB. It acts as a container
/// for subsystem-specific configurations (WAL, Historical, Vector Indexes).
pub struct AletheiaDBConfig {"""
content = content.replace(target, replacement)

with open("src/config.rs", "w") as f:
    f.write(content)
