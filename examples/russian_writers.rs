//! Russian Writers Knowledge Graph - Comprehensive AletheiaDB Demo
//!
//! This example demonstrates AletheiaDB's capabilities using Russian literary history:
//! - Bi-temporal storage with evolving interpretations
//! - Vector embeddings for semantic search
//! - Hybrid graph traversal + vector similarity queries
//! - Rich relational data with real educational value
//!
//! Prerequisites:
//! 1. Run the data fetcher: cd examples/russian_writers && python fetch_data.py
//! 2. Ensure Ollama is running with all-minilm model
//!
//! Run with: cargo run --example russian_writers

use aletheiadb::{
    AletheiaDB, GLOBAL_INTERNER, InternedString, NodeId, PropertyMapBuilder, Result, Timestamp,
    WriteOps, query::semantic_pathfinding::SemanticPathfinder,
};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

// ============================================================================
// Data Structures (matching Python fetcher output)
// ============================================================================

#[derive(Debug, Deserialize, Serialize)]
struct Author {
    name: String,
    birth_year: i64,
    death_year: i64,
    nationality: String,
    biography: String,
    writing_style: String,
    major_themes: String,
    wikipedia_url: String,
    #[serde(default)]
    style_embedding: Option<Vec<f32>>,
    #[serde(default)]
    semantic_embedding: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Book {
    title: String,
    original_title: String,
    author: String,
    published_year: i64,
    genre: String,
    summary: String,
    themes: String,
    critical_reception: String,
    interpretation: String,
    wikipedia_url: String,
    #[serde(default)]
    theme_embedding: Option<Vec<f32>>,
    #[serde(default)]
    semantic_embedding: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Character {
    name: String,
    book: String,
    author: String,
    role: String,
    description: String,
    personality: String,
    arc: String,
    significance: String,
    #[serde(default)]
    personality_embedding: Option<Vec<f32>>,
    #[serde(default)]
    semantic_embedding: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Theme {
    name: String,
    description: String,
    examples: String,
    #[serde(default)]
    theme_embedding: Option<Vec<f32>>,
    #[serde(default)]
    semantic_embedding: Option<Vec<f32>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Movement {
    name: String,
    period: String,
    characteristics: String,
    key_figures: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct HistoricalEvent {
    name: String,
    year: i64,
    description: String,
    significance: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Relationships {
    wrote: Vec<WroteRelation>,
    influenced_by: Vec<InfluencedByRelation>,
    appears_in: Vec<AppearsInRelation>,
    contains_theme: Vec<ContainsThemeRelation>,
    similar_to: Vec<SimilarToRelation>,
}

#[derive(Debug, Deserialize, Serialize)]
struct WroteRelation {
    author: String,
    book: String,
    year: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct InfluencedByRelation {
    source: String,
    target: String,
    #[serde(rename = "type")]
    influence_type: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct AppearsInRelation {
    character: String,
    book: String,
    role: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ContainsThemeRelation {
    book: String,
    theme: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct SimilarToRelation {
    character1: String,
    character2: String,
    #[serde(rename = "type")]
    similarity_type: String,
}

// ============================================================================
// Auto-complete Helper
// ============================================================================

struct RussianLitCompleter {
    commands: Vec<String>,
    authors: Vec<String>,
    books: Vec<String>,
    characters: Vec<String>,
}

impl RussianLitCompleter {
    fn new(demo: &DemoData) -> Self {
        Self {
            commands: vec![
                "similar".to_string(),
                "sim".to_string(),
                "styles".to_string(),
                "archetypes".to_string(),
                "thematic".to_string(),
                "path".to_string(),
                "timewarp".to_string(),
                "tw".to_string(),
                "influences".to_string(),
                "inf".to_string(),
                "drift".to_string(),
                "evolution".to_string(),
                "evo".to_string(),
                "indexes".to_string(),
                "list authors".to_string(),
                "list books".to_string(),
                "list characters".to_string(),
                "list themes".to_string(),
                "stats".to_string(),
                "timing".to_string(),
                "help".to_string(),
                "quit".to_string(),
                "exit".to_string(),
            ],
            authors: demo.authors.keys().cloned().collect(),
            books: demo.books.keys().cloned().collect(),
            characters: demo.characters.keys().cloned().collect(),
        }
    }
}

impl Completer for RussianLitCompleter {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let line = &line[..pos];
        let mut candidates = Vec::new();

        // Split into command and args
        let parts: Vec<&str> = line.splitn(2, ' ').collect();

        if parts.len() == 1 {
            // Completing command
            let prefix = parts[0].to_lowercase();
            for cmd in &self.commands {
                if cmd.to_lowercase().starts_with(&prefix) {
                    candidates.push(Pair {
                        display: cmd.clone(),
                        replacement: cmd.clone(),
                    });
                }
            }
        } else {
            // Completing arguments
            let cmd = parts[0];
            let arg_prefix = parts[1].to_lowercase();

            match cmd {
                "similar" | "sim" => {
                    for name in &self.characters {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: name.clone(),
                                replacement: name.clone(),
                            });
                        }
                    }
                }
                "styles" => {
                    for name in &self.authors {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: name.clone(),
                                replacement: name.clone(),
                            });
                        }
                    }
                }
                "archetypes" => {
                    for name in &self.characters {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: name.clone(),
                                replacement: name.clone(),
                            });
                        }
                    }
                }
                "thematic" => {
                    for name in &self.books {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: format!("\"{}\"", name),
                                replacement: format!("\"{}\" ", name),
                            });
                        }
                    }
                }
                "influences" | "inf" => {
                    for name in &self.authors {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: name.clone(),
                                replacement: name.clone(),
                            });
                        }
                    }
                }
                "timewarp" | "tw" => {
                    for name in &self.books {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: format!("\"{}\"", name),
                                replacement: format!("\"{}\" ", name),
                            });
                        }
                    }
                }
                "drift" => {
                    for name in &self.characters {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: name.clone(),
                                replacement: name.clone(),
                            });
                        }
                    }
                }
                "evolution" | "evo" => {
                    for name in &self.books {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: format!("\"{}\"", name),
                                replacement: format!("\"{}\" ", name),
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        Ok((0, candidates))
    }
}

impl Hinter for RussianLitCompleter {
    type Hint = String;
}

impl Highlighter for RussianLitCompleter {}

impl Validator for RussianLitCompleter {}

impl Helper for RussianLitCompleter {}

// ============================================================================
// Demo State
// ============================================================================

struct DemoData {
    db: AletheiaDB,
    // Track entities by type for easy lookup
    authors: HashMap<String, NodeId>,
    books: HashMap<String, NodeId>,
    characters: HashMap<String, NodeId>,
    themes: HashMap<String, NodeId>,
    // Performance timing mode
    timing_enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl DemoData {
    fn new() -> Self {
        Self {
            db: AletheiaDB::new().expect("Failed to create database"),
            authors: HashMap::new(),
            books: HashMap::new(),
            characters: HashMap::new(),
            themes: HashMap::new(),
            timing_enabled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Lookup node by name across all entity types
    fn get_node(&self, name: &str) -> Option<NodeId> {
        self.authors
            .get(name)
            .or_else(|| self.books.get(name))
            .or_else(|| self.characters.get(name))
            .or_else(|| self.themes.get(name))
            .copied()
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Helper to get label string from InternedString
fn label_str(label: InternedString) -> String {
    GLOBAL_INTERNER
        .resolve_with(label, |s| s.to_string())
        .unwrap_or_else(|| format!("{:?}", label))
}

/// Wrap text at word boundaries with indentation for continuation lines
///
/// Example: wrap_text("This is a long text", 10, "  ") →
///   "This is a\n  long text"
fn wrap_text(text: &str, width: usize, indent: &str) -> String {
    let mut result = String::new();
    let mut current_line = String::new();
    let mut first_line = true;

    for word in text.split_whitespace() {
        let word_len = word.len();
        let current_len = current_line.len();

        // Check if adding this word would exceed width
        if current_len > 0 && current_len + 1 + word_len > width {
            // Flush current line and start a new one
            if !first_line {
                result.push_str(indent);
            }
            result.push_str(&current_line);
            result.push('\n');
            current_line.clear();
            first_line = false;
        }

        if !current_line.is_empty() {
            current_line.push(' ');
        }
        current_line.push_str(word);
    }

    // Add the last line
    if !current_line.is_empty() {
        if !first_line {
            result.push_str(indent);
        }
        result.push_str(&current_line);
    }

    result
}

/// Parse command arguments respecting quoted strings
///
/// Example: `"Crime and Punishment" 1900` → ["Crime and Punishment", "1900"]
fn parse_quoted_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current_arg = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
            }
            ' ' if !in_quotes => {
                if !current_arg.is_empty() {
                    args.push(current_arg.trim().to_string());
                    current_arg.clear();
                }
            }
            _ => {
                current_arg.push(ch);
            }
        }
    }

    if !current_arg.is_empty() {
        args.push(current_arg.trim().to_string());
    }

    args
}

/// Find a character by name with fuzzy matching
///
/// Supports:
/// - Exact match: "Rodion Raskolnikov"
/// - Last name only: "Raskolnikov"
/// - Case-insensitive: "raskolnikov"
/// - Partial match: "Raskol"
fn find_character_fuzzy<'a>(
    characters: &'a HashMap<String, NodeId>,
    query: &str,
) -> Option<(&'a String, &'a NodeId)> {
    let query_lower = query.to_lowercase();

    // Try exact match first (case-insensitive)
    for (name, id) in characters.iter() {
        if name.to_lowercase() == query_lower {
            return Some((name, id));
        }
    }

    // Try last name match (handles "Raskolnikov" for "Rodion Raskolnikov")
    for (name, id) in characters.iter() {
        if name.split_whitespace().last().map(|s| s.to_lowercase()) == Some(query_lower.clone()) {
            return Some((name, id));
        }
    }

    // Try partial match (case-insensitive substring)
    for (name, id) in characters.iter() {
        if name.to_lowercase().contains(&query_lower) {
            return Some((name, id));
        }
    }

    None
}

/// Strip surrounding quotes from a string (handles both " and ')
fn strip_quotes(s: &str) -> &str {
    let trimmed = s.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    }
}

/// Fuzzy search for entity name in a HashMap (authors, books, themes)
///
/// Tries: exact match → last name match → partial match
fn find_entity_fuzzy<'a>(
    entities: &'a HashMap<String, NodeId>,
    query: &str,
) -> Option<(&'a String, &'a NodeId)> {
    let query_lower = query.to_lowercase();

    // Try exact match first (case-insensitive)
    for (name, id) in entities.iter() {
        if name.to_lowercase() == query_lower {
            return Some((name, id));
        }
    }

    // Try last name match (handles "Dostoevsky" for "Fyodor Dostoevsky")
    for (name, id) in entities.iter() {
        if name.split_whitespace().last().map(|s| s.to_lowercase()) == Some(query_lower.clone()) {
            return Some((name, id));
        }
    }

    // Try partial match (case-insensitive substring)
    for (name, id) in entities.iter() {
        if name.to_lowercase().contains(&query_lower) {
            return Some((name, id));
        }
    }

    None
}

/// Search for an entity across all types (authors, books, characters, themes)
/// Returns (name, node_id, entity_type)
fn find_any_entity(demo: &DemoData, query: &str) -> Option<(String, NodeId, &'static str)> {
    // Try authors first
    if let Some((name, &id)) = find_entity_fuzzy(&demo.authors, query) {
        return Some((name.clone(), id, "Author"));
    }

    // Try books
    if let Some((name, &id)) = find_entity_fuzzy(&demo.books, query) {
        return Some((name.clone(), id, "Book"));
    }

    // Try characters
    if let Some((name, &id)) = find_character_fuzzy(&demo.characters, query) {
        return Some((name.clone(), id, "Character"));
    }

    // Try themes
    if let Some((name, &id)) = find_entity_fuzzy(&demo.themes, query) {
        return Some((name.clone(), id, "Theme"));
    }

    None
}

/// Macro to time an operation and print elapsed time if timing is enabled
macro_rules! timed {
    ($demo:expr, $label:expr, $op:expr) => {{
        let start = std::time::Instant::now();
        let result = $op;
        if $demo
            .timing_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let elapsed = start.elapsed();
            println!(
                "  ⏱️  {} took {:.3}ms",
                $label,
                elapsed.as_secs_f64() * 1000.0
            );
        }
        result
    }};
}

/// Get the display name from any entity node
///
/// Tries 'name' property first, then 'title' property
fn get_entity_name(node: &aletheiadb::core::graph::Node) -> Result<String> {
    node.properties
        .get("name")
        .or_else(|| node.properties.get("title"))
        .map(format_value)
        .ok_or_else(|| aletheiadb::Error::other("Entity has no name or title property".to_string()))
}

/// Get current timestamp
fn now_timestamp() -> Result<Timestamp> {
    Ok(aletheiadb::core::temporal::time::now())
}

/// Create approximate timestamp for Jan 1 of a given year
///
/// NOTE: This is an approximation that doesn't account for leap years.
/// It's sufficient for demonstration purposes but shouldn't be used
/// for production temporal queries. Use a proper datetime library like
/// `chrono` for accurate timestamp conversion.
fn year_to_timestamp(year: i64) -> Timestamp {
    // Rough approximation: milliseconds since epoch for Jan 1 of that year
    // Average 365.25 days/year to approximate leap years
    let years_since_1970 = year.saturating_sub(1970);
    let days = (years_since_1970 * 365) + (years_since_1970 / 4);
    let millis = days.saturating_mul(86400).saturating_mul(1_000);
    aletheiadb::core::temporal::time::from_millis(millis)
}

/// Helper to format property values nicely
fn format_value(value: &aletheiadb::PropertyValue) -> String {
    use aletheiadb::PropertyValue;
    match value {
        PropertyValue::Null => "null".to_string(),
        PropertyValue::Bool(b) => b.to_string(),
        PropertyValue::Int(i) => i.to_string(),
        PropertyValue::Float(f) => f.to_string(),
        PropertyValue::String(s) => s.to_string(),
        PropertyValue::Bytes(b) => format!("<{} bytes>", b.len()),
        PropertyValue::Array(arr) => format!("[{} items]", arr.len()),
        PropertyValue::Vector(v) => format!("<vector dim={}>", v.len()),
        PropertyValue::SparseVector(v) => format!("<sparse vector, {} nnz>", v.nnz()),
    }
}

/// Helper to create properties more easily
macro_rules! props {
    ($($key:expr => $value:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut builder = PropertyMapBuilder::new();
        $(builder = builder.insert($key, $value);)*
        builder.build()
    }};
}

// ============================================================================
// Data Loading
// ============================================================================

fn load_json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path).map_err(aletheiadb::Error::Io)?;

    serde_json::from_str(&content)
        .map_err(|e| aletheiadb::Error::other(format!("JSON deserialization failed: {}", e)))
}

fn check_data_files() -> Result<()> {
    let data_dir = Path::new("examples/russian_writers/data");

    if !data_dir.exists() {
        return Err(aletheiadb::Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Data directory not found: {}\n\
                Please run the data fetcher first:\n\
                  cd examples/russian_writers\n\
                  python fetch_data.py",
                data_dir.display()
            ),
        )));
    }

    let required_files = [
        "authors.json",
        "books.json",
        "characters.json",
        "themes.json",
        "movements.json",
        "events.json",
        "relationships.json",
    ];

    for file in &required_files {
        let path = data_dir.join(file);
        if !path.exists() {
            return Err(aletheiadb::Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Required file not found: {}", path.display()),
            )));
        }
    }

    Ok(())
}

// ============================================================================
// Database Population
// ============================================================================

fn populate_database(demo: &mut DemoData) -> Result<()> {
    println!("\n>>> Loading Russian Literary History...\n");

    let data_dir = Path::new("examples/russian_writers/data");

    // Load all JSON data
    println!("  Loading data files...");
    let authors: Vec<Author> = load_json_file(&data_dir.join("authors.json"))?;
    let books: Vec<Book> = load_json_file(&data_dir.join("books.json"))?;
    let characters: Vec<Character> = load_json_file(&data_dir.join("characters.json"))?;
    let themes: Vec<Theme> = load_json_file(&data_dir.join("themes.json"))?;
    let movements: Vec<Movement> = load_json_file(&data_dir.join("movements.json"))?;
    let events: Vec<HistoricalEvent> = load_json_file(&data_dir.join("events.json"))?;
    let relationships: Relationships = load_json_file(&data_dir.join("relationships.json"))?;

    println!("  ✓ Loaded {} authors", authors.len());
    println!("  ✓ Loaded {} books", books.len());
    println!("  ✓ Loaded {} characters", characters.len());
    println!("  ✓ Loaded {} themes", themes.len());
    println!("  ✓ Loaded {} movements", movements.len());
    println!("  ✓ Loaded {} events", events.len());

    // === ENABLE VECTOR INDEXING ===
    // IMPORTANT: Must enable BEFORE creating nodes so embeddings are automatically indexed
    // Enable both current-state and temporal indexes:
    // 1. Current index: for fast similarity searches (find_similar)
    // 2. Temporal index: for tracking semantic drift over time
    println!("\n  Setting up vector indexes...");
    use aletheiadb::index::vector::temporal::{
        RetentionPolicy, SnapshotStrategy, TemporalVectorConfig,
    };
    use aletheiadb::index::vector::{DistanceMetric, HnswConfig};

    let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);

    // Enable semantic_embedding index for cross-entity similarity
    demo.db
        .vector_index("semantic_embedding")
        .hnsw(hnsw_config.clone())
        .enable()?;
    println!("    ✓ semantic_embedding (cross-entity similarity)");

    // Enable style_embedding index for author writing style analysis
    demo.db
        .vector_index("style_embedding")
        .hnsw(hnsw_config.clone())
        .enable()?;
    println!("    ✓ style_embedding (Authors, literary style analysis)");

    // Enable theme_embedding index for thematic content similarity
    demo.db
        .vector_index("theme_embedding")
        .hnsw(hnsw_config.clone())
        .enable()?;
    println!("    ✓ theme_embedding (Books + Themes, thematic analysis)");

    // Enable personality_embedding index for character archetype similarity
    demo.db
        .vector_index("personality_embedding")
        .hnsw(hnsw_config.clone())
        .enable()?;
    println!("    ✓ personality_embedding (Characters, character analysis)");

    // Show active indexes
    let indexes = demo.db.list_vector_indexes();
    println!("\n  Active vector indexes: {}", indexes.len());
    for idx in &indexes {
        println!(
            "    • {}: {} dims, metric: {:?}",
            idx.property_name, idx.dimensions, idx.distance_metric
        );
    }

    // Enable temporal vector index for semantic drift tracking
    // Only track semantic_embedding temporally (for character evolution demo)
    let temporal_config = TemporalVectorConfig {
        hnsw_config: Some(hnsw_config),
        // Snapshot strategy aligned with graph anchor_interval (both set to 2)
        // Snapshots are created via pre-anchor hooks, so they fire when anchors are created
        // With anchor_interval=2 and TransactionInterval(2), snapshots created every 2 versions
        snapshot_strategy: SnapshotStrategy::TransactionInterval(2),
        retention_policy: RetentionPolicy::KeepAll, // Keep all snapshots for demo
        max_snapshots: 100,                         // Maximum snapshots to retain
        full_snapshot_interval: 5,                  // Full snapshot every 5 snapshots
    };

    demo.db
        .enable_temporal_vector_index("semantic_embedding", temporal_config)?;
    println!("    ✓ Temporal vector index enabled for semantic_embedding");
    println!("    ✓ Snapshots every 2 transactions to capture semantic drift");

    // === CREATE AUTHORS ===
    println!("\n  Creating Authors...");
    for author in authors {
        let mut builder = PropertyMapBuilder::new()
            .insert("name", author.name.as_str())
            .insert("birth_year", author.birth_year)
            .insert("death_year", author.death_year)
            .insert("nationality", author.nationality.as_str())
            .insert("biography", author.biography.as_str())
            .insert("writing_style", author.writing_style.as_str())
            .insert("major_themes", author.major_themes.as_str())
            .insert("wikipedia_url", author.wikipedia_url.as_str());

        // Add embeddings if present
        if let Some(embedding) = author.style_embedding {
            builder = builder.insert_vector("style_embedding", &embedding);
        }
        if let Some(embedding) = author.semantic_embedding {
            builder = builder.insert_vector("semantic_embedding", &embedding);
        }

        let node_id = demo.db.create_node("Author", builder.build())?;
        demo.authors.insert(author.name, node_id);
    }

    // === CREATE BOOKS ===
    println!("  Creating Books...");
    for book in books {
        let mut builder = PropertyMapBuilder::new()
            .insert("title", book.title.as_str())
            .insert("original_title", book.original_title.as_str())
            .insert("author", book.author.as_str())
            .insert("published_year", book.published_year)
            .insert("genre", book.genre.as_str())
            .insert("summary", book.summary.as_str())
            .insert("themes", book.themes.as_str())
            .insert("critical_reception", book.critical_reception.as_str())
            .insert("interpretation", book.interpretation.as_str())
            .insert("wikipedia_url", book.wikipedia_url.as_str());

        if let Some(embedding) = book.theme_embedding {
            builder = builder.insert_vector("theme_embedding", &embedding);
        }
        if let Some(embedding) = book.semantic_embedding {
            builder = builder.insert_vector("semantic_embedding", &embedding);
        }

        let node_id = demo.db.create_node("Book", builder.build())?;
        demo.books.insert(book.title.clone(), node_id);
    }

    // === CREATE CHARACTERS ===
    println!("  Creating Characters...");
    for character in characters {
        let mut builder = PropertyMapBuilder::new()
            .insert("name", character.name.as_str())
            .insert("book", character.book.as_str())
            .insert("author", character.author.as_str())
            .insert("role", character.role.as_str())
            .insert("description", character.description.as_str())
            .insert("personality", character.personality.as_str())
            .insert("arc", character.arc.as_str())
            .insert("significance", character.significance.as_str());

        if let Some(embedding) = character.personality_embedding {
            builder = builder.insert_vector("personality_embedding", &embedding);
        }
        if let Some(embedding) = character.semantic_embedding {
            builder = builder.insert_vector("semantic_embedding", &embedding);
        }

        let node_id = demo.db.create_node("Character", builder.build())?;
        demo.characters.insert(character.name, node_id);
    }

    // === CREATE THEMES ===
    println!("  Creating Themes...");
    for theme in themes {
        let mut builder = PropertyMapBuilder::new()
            .insert("name", theme.name.as_str())
            .insert("description", theme.description.as_str())
            .insert("examples", theme.examples.as_str());

        if let Some(embedding) = theme.theme_embedding {
            builder = builder.insert_vector("theme_embedding", &embedding);
        }
        if let Some(embedding) = theme.semantic_embedding {
            builder = builder.insert_vector("semantic_embedding", &embedding);
        }

        let node_id = demo.db.create_node("Theme", builder.build())?;
        demo.themes.insert(theme.name, node_id);
    }

    // === CREATE MOVEMENTS ===
    println!("  Creating Literary Movements...");
    for movement in movements {
        let _node_id = demo.db.create_node(
            "Movement",
            props! {
                "name" => movement.name.as_str(),
                "period" => movement.period.as_str(),
                "characteristics" => movement.characteristics.as_str(),
                "key_figures" => movement.key_figures.as_str(),
            },
        )?;
        // Note: Movements are not tracked by name since they're not queried individually
    }

    // === CREATE HISTORICAL EVENTS ===
    println!("  Creating Historical Events...");
    for event in events {
        let _node_id = demo.db.create_node(
            "HistoricalEvent",
            props! {
                "name" => event.name.as_str(),
                "year" => event.year,
                "description" => event.description.as_str(),
                "significance" => event.significance.as_str(),
            },
        )?;
        // Note: Historical events are not tracked by name since they're not queried individually
    }

    // === CREATE RELATIONSHIPS ===
    println!("\n  Creating Relationships...");

    // WROTE relationships
    for rel in relationships.wrote {
        if let (Some(&author_id), Some(&book_id)) =
            (demo.authors.get(&rel.author), demo.books.get(&rel.book))
        {
            demo.db
                .create_edge(author_id, book_id, "WROTE", props! { "year" => rel.year })?;
        }
    }

    // INFLUENCED_BY relationships
    for rel in relationships.influenced_by {
        if let (Some(&source_id), Some(&target_id)) =
            (demo.authors.get(&rel.source), demo.authors.get(&rel.target))
        {
            demo.db.create_edge(
                source_id,
                target_id,
                "INFLUENCED_BY",
                props! { "type" => rel.influence_type.as_str() },
            )?;
        }
    }

    // APPEARS_IN relationships
    for rel in relationships.appears_in {
        if let (Some(&char_id), Some(&book_id)) = (
            demo.characters.get(&rel.character),
            demo.books.get(&rel.book),
        ) {
            demo.db.create_edge(
                char_id,
                book_id,
                "APPEARS_IN",
                props! { "role" => rel.role.as_str() },
            )?;
        }
    }

    // CONTAINS_THEME relationships
    for rel in relationships.contains_theme {
        if let (Some(&book_id), Some(&theme_id)) =
            (demo.books.get(&rel.book), demo.themes.get(&rel.theme))
        {
            demo.db
                .create_edge(book_id, theme_id, "CONTAINS_THEME", props! {})?;
        }
    }

    // SIMILAR_TO relationships (bidirectional for characters)
    for rel in relationships.similar_to {
        if let (Some(&char1_id), Some(&char2_id)) = (
            demo.characters.get(&rel.character1),
            demo.characters.get(&rel.character2),
        ) {
            demo.db.create_edge(
                char1_id,
                char2_id,
                "SIMILAR_TO",
                props! { "type" => rel.similarity_type.as_str() },
            )?;
            // Create reverse edge too
            demo.db.create_edge(
                char2_id,
                char1_id,
                "SIMILAR_TO",
                props! { "type" => rel.similarity_type.as_str() },
            )?;
        }
    }

    println!(
        "\n>>> Database populated with {} nodes and {} edges!",
        demo.db.node_count(),
        demo.db.edge_count()
    );

    Ok(())
}

// ============================================================================
// Temporal Evolution - Simulate Evolving Interpretations
// ============================================================================

fn create_temporal_versions(demo: &mut DemoData) -> Result<()> {
    println!("\n>>> Creating temporal history (evolving literary interpretations)...\n");

    // Define evolving interpretations for key books
    let evolutions = [
        (
            "Crime and Punishment",
            vec![
                (
                    1885,
                    "Recognized as masterwork of psychological realism; exploration of guilt",
                ),
                (
                    1925,
                    "Precursor to existentialist philosophy; themes of alienation and absurdity",
                ),
                (
                    1955,
                    "Freudian analysis: Oedipal conflict, unconscious guilt, id vs superego",
                ),
                (
                    2024,
                    "Complex portrayal of mental illness, poverty, and social isolation; trauma studies",
                ),
            ],
        ),
        (
            "Anna Karenina",
            vec![
                (1885, "Morality tale about adultery and its consequences"),
                (
                    1925,
                    "Psychological study of passion vs duty; feminist themes emerge",
                ),
                (
                    1960,
                    "Critique of patriarchal society and women's limited choices",
                ),
                (
                    2024,
                    "Complex exploration of agency, mental health, and societal judgment",
                ),
            ],
        ),
        (
            "The Brothers Karamazov",
            vec![
                (
                    1900,
                    "Philosophical novel exploring faith, doubt, and morality",
                ),
                (
                    1945,
                    "Post-war: existential questions of God's existence and human suffering",
                ),
                (
                    1970,
                    "Psychological depth: family dysfunction, parricide, unconscious motivations",
                ),
                (
                    2024,
                    "Multilayered exploration of faith, ethics, psychology, and family trauma",
                ),
            ],
        ),
    ];

    for (book_title, stages) in &evolutions {
        if let Some(&book_id) = demo.books.get(*book_title) {
            println!("  Evolving interpretation: {}", book_title);

            for (year, interpretation) in stages {
                // Create a new version with updated interpretation
                // Note: update_node does a full replacement (PUT), so we need all properties
                let current_node = demo.db.get_node(book_id)?;

                // Rebuild PropertyMap with all existing properties plus updated interpretation
                let mut builder = PropertyMapBuilder::new();
                for (key, value) in current_node.properties.iter() {
                    let key_str = label_str(*key);
                    if key_str == "interpretation" {
                        builder = builder.insert("interpretation", *interpretation);
                    } else {
                        builder = builder.insert_by_key(*key, value.clone());
                    }
                }

                demo.db
                    .write(|tx| tx.update_node(book_id, builder.build()))?;

                // Truncate interpretation (character-aware)
                let truncated: String = interpretation.chars().take(60).collect();
                println!("    {} → {}", year, truncated);

                // Small delay to ensure distinct timestamps
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    println!("\n  ✓ Created temporal versions showing evolution of literary criticism");

    // === EVOLVE CHARACTER PERSONALITIES (WITH EMBEDDINGS) ===
    // Demonstrate semantic drift: how our understanding of character personalities evolved
    println!("\n  Evolving character personalities (semantic drift demo)...\n");

    let character_evolutions = [
        (
            "Rodion Raskolnikov",
            vec![
                (
                    1900,
                    "Psychologically complex anti-hero torn by guilt and moral struggle. \
                     Dostoevsky's masterful portrayal of a tormented soul.",
                ),
                (
                    1950,
                    "Existential hero struggling with absurdity and alienation. Precursor to \
                     Camus and Sartre. Emblematic of modern anxiety.",
                ),
                (
                    2024,
                    "Trauma survivor with mental illness driven by poverty and social isolation. \
                     Complex portrayal of desperation and psychological breakdown.",
                ),
            ],
        ),
        (
            "Anna Karenina",
            vec![
                (
                    1925,
                    "Passionate woman constrained by rigid social conventions. Feminist reading \
                     emphasizes her quest for authentic love.",
                ),
                (
                    1960,
                    "Victim of patriarchal oppression and limited choices. Critique of marriage \
                     as economic transaction. Tragic heroine.",
                ),
                (
                    2024,
                    "Complex individual struggling with mental health, societal judgment, and \
                     limited agency. Modern empathy for her psychological state.",
                ),
            ],
        ),
        (
            "Prince Myshkin",
            vec![
                (
                    1920,
                    "Christ-like figure of pure goodness and compassion. Embodies Dostoevsky's \
                     moral ideals. Tragedy of innocence.",
                ),
                (
                    1970,
                    "Psychological study of neurodivergence and epilepsy. Outsider perspective \
                     on corrupt society. Misunderstood visionary.",
                ),
                (
                    2024,
                    "Neurodivergent individual navigating social expectations. Authentic self in \
                     world of performative behavior. Disability representation.",
                ),
            ],
        ),
    ];

    for (character_name, stages) in &character_evolutions {
        if let Some(&character_id) = demo.characters.get(*character_name) {
            println!("  Evolving understanding: {}", character_name);

            // Get original embedding to perturb
            let original_node = demo.db.get_node(character_id)?;
            let original_embedding = if let Some(aletheiadb::PropertyValue::Vector(vec)) =
                original_node.properties.get("semantic_embedding")
            {
                vec.clone()
            } else {
                println!("    ⚠️  No semantic_embedding found, skipping");
                continue;
            };

            for (year, evolved_personality) in stages {
                // Simulate semantic drift by perturbing the original embedding
                // This mimics how our understanding of the character evolved over time
                let drift_factor = (*year as f32 - 1866.0) / 1000.0; // 0.0 to ~0.16
                let perturbed_embedding: Vec<f32> = original_embedding
                    .iter()
                    .enumerate()
                    .map(|(i, &val)| {
                        // Add controlled perturbation based on year
                        let noise = ((i as f32 * drift_factor).sin()) * 0.15;
                        (val + noise).clamp(-1.0, 1.0)
                    })
                    .collect();

                // Note: update_node does a full replacement (PUT), so we need all properties
                // Get current node to preserve all existing properties
                let current_node = demo.db.get_node(character_id)?;

                // Rebuild PropertyMap with all existing properties plus updated personality and embedding
                let mut builder = PropertyMapBuilder::new();
                for (key, value) in current_node.properties.iter() {
                    let key_str = label_str(*key);
                    if key_str == "personality" {
                        builder = builder.insert("personality", *evolved_personality);
                    } else if key_str == "semantic_embedding" {
                        builder = builder.insert_vector("semantic_embedding", &perturbed_embedding);
                    } else {
                        builder = builder.insert_by_key(*key, value.clone());
                    }
                }

                demo.db
                    .write(|tx| tx.update_node(character_id, builder.build()))?;

                // Truncate personality (character-aware)
                let truncated: String = evolved_personality.chars().take(60).collect();
                println!("    {} → {}", year, truncated);

                // Small delay to ensure distinct timestamps
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    println!("\n  ✓ Created temporal versions with semantic drift in embeddings");

    Ok(())
}

// ============================================================================
// Query Functions
// ============================================================================

fn show_entity(demo: &DemoData, name: &str) -> Result<()> {
    if let Some(node_id) = demo.get_node(name) {
        let node = demo.db.get_node(node_id)?;

        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!("║  {}  ", name.to_uppercase());
        println!("╚═══════════════════════════════════════════════════════════╝");
        println!("\nLabel: {}", label_str(node.label));
        println!("\nProperties:");

        // Sort properties for consistent display
        let mut props: Vec<_> = node.properties.iter().collect();
        props.sort_by_key(|(k, _)| {
            GLOBAL_INTERNER
                .resolve_with(**k, |s| s.to_string())
                .unwrap_or_default()
        });

        for (key, value) in props {
            let key_str = GLOBAL_INTERNER
                .resolve_with(*key, |s| s.to_string())
                .unwrap_or_else(|| format!("{:?}", key));

            let value_str = format_value(value);

            // Truncate long values (character-aware, not byte-aware)
            if value_str.chars().count() > 100 {
                let truncated: String = value_str.chars().take(97).collect();
                println!("  {}: {}...", key_str, truncated);
            } else {
                println!("  {}: {}", key_str, value_str);
            }
        }

        // Show relationships
        let incoming = demo.db.get_incoming_edges(node_id);
        let outgoing = demo.db.get_outgoing_edges(node_id);

        if !incoming.is_empty() {
            println!("\nIncoming relationships:");
            for edge_id in incoming.iter().take(10) {
                if let Ok(edge) = demo.db.get_edge(*edge_id) {
                    let source = demo.db.get_node(edge.source)?;
                    let source_name = source
                        .properties
                        .get("name")
                        .or_else(|| source.properties.get("title"))
                        .map(format_value)
                        .unwrap_or_else(|| label_str(source.label));
                    println!("  {} --[{}]-->", source_name, label_str(edge.label));
                }
            }
            if incoming.len() > 10 {
                println!("  ... and {} more", incoming.len() - 10);
            }
        }

        if !outgoing.is_empty() {
            println!("\nOutgoing relationships:");
            for edge_id in outgoing.iter().take(10) {
                if let Ok(edge) = demo.db.get_edge(*edge_id) {
                    let target = demo.db.get_node(edge.target)?;
                    let target_name = target
                        .properties
                        .get("name")
                        .or_else(|| target.properties.get("title"))
                        .map(format_value)
                        .unwrap_or_else(|| label_str(target.label));
                    println!("  --[{}]--> {}", label_str(edge.label), target_name);
                }
            }
            if outgoing.len() > 10 {
                println!("  ... and {} more", outgoing.len() - 10);
            }
        }
    } else {
        println!("\n❌ Entity not found: {}", name);
        println!("\nTry: list authors, list books, list characters");
    }

    Ok(())
}

fn list_entities(demo: &DemoData, entity_type: &str) -> Result<()> {
    let (label, map) = match entity_type {
        "authors" => ("Author", &demo.authors),
        "books" => ("Book", &demo.books),
        "characters" => ("Character", &demo.characters),
        "themes" => ("Theme", &demo.themes),
        _ => {
            println!("Unknown entity type. Try: authors, books, characters, themes");
            return Ok(());
        }
    };

    println!("\n═══ {} ({})", label.to_uppercase(), map.len());

    let mut items: Vec<_> = map.iter().collect();
    items.sort_by_key(|(name, _)| *name);

    for (name, &id) in items {
        if let Ok(node) = demo.db.get_node(id) {
            // Show brief info based on type
            match entity_type {
                "authors" => {
                    let years = format!(
                        "{}-{}",
                        node.properties
                            .get("birth_year")
                            .map(format_value)
                            .unwrap_or_default(),
                        node.properties
                            .get("death_year")
                            .map(format_value)
                            .unwrap_or_default()
                    );
                    println!("  • {} ({})", name, years);
                }
                "books" => {
                    let year = node
                        .properties
                        .get("published_year")
                        .map(format_value)
                        .unwrap_or_default();
                    let author = node
                        .properties
                        .get("author")
                        .map(format_value)
                        .unwrap_or_default();
                    println!("  • {} ({}) - {}", name, year, author);
                }
                "characters" => {
                    let book = node
                        .properties
                        .get("book")
                        .map(format_value)
                        .unwrap_or_default();
                    println!("  • {} from {}", name, book);
                }
                "themes" => {
                    println!("  • {}", name);
                }
                _ => {}
            }
        }
    }

    Ok(())
}

fn show_stats(demo: &DemoData) -> Result<()> {
    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  DATABASE STATISTICS");
    println!("╚═══════════════════════════════════════════════════════════╝");

    println!("\nCurrent State:");
    println!("  Total nodes: {}", demo.db.node_count());
    println!("  Total edges: {}", demo.db.edge_count());
    println!("  Authors: {}", demo.authors.len());
    println!("  Books: {}", demo.books.len());
    println!("  Characters: {}", demo.characters.len());
    println!("  Themes: {}", demo.themes.len());

    let stats = demo.db.historical_stats()?;
    println!("\nHistorical Storage:");
    println!("  Total node versions: {}", stats.total_node_versions);
    println!("  Total edge versions: {}", stats.total_edge_versions);
    println!("  Node anchors: {}", stats.node_anchor_count);
    println!("  Node deltas: {}", stats.node_delta_count);
    println!("  Edge anchors: {}", stats.edge_anchor_count);
    println!("  Edge deltas: {}", stats.edge_delta_count);

    if stats.node_delta_count > 0 {
        println!(
            "\n  Compression ratio: {:.1}%",
            stats.compression_ratio() * 100.0
        );
    }

    Ok(())
}

fn find_similar_entities(
    demo: &DemoData,
    entity_name: &str,
    k: usize,
    type_filter: Option<&str>,
    property_name: Option<&str>,
) -> Result<()> {
    // Try to find the query entity across all types (with fuzzy matching)
    let (query_id, query_label) = find_character_fuzzy(&demo.characters, entity_name)
        .map(|(name, id)| (*id, name.clone()))
        .or_else(|| {
            find_entity_fuzzy(&demo.authors, entity_name).map(|(name, id)| (*id, name.clone()))
        })
        .or_else(|| {
            find_entity_fuzzy(&demo.books, entity_name).map(|(name, id)| (*id, name.clone()))
        })
        .or_else(|| {
            find_entity_fuzzy(&demo.themes, entity_name).map(|(name, id)| (*id, name.clone()))
        })
        .ok_or_else(|| aletheiadb::Error::other(format!("Entity not found: {}", entity_name)))?;

    let query_node = demo.db.get_node(query_id)?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  SEMANTIC SIMILARITY: {}", query_label.to_uppercase());
    println!("╚═══════════════════════════════════════════════════════════╝");

    // Display query entity info
    println!(
        "\nQuery entity: {} ({})",
        query_label,
        label_str(query_node.label)
    );
    if entity_name.to_lowercase() != query_label.to_lowercase() {
        println!("  (matched '{}' to '{}')", entity_name, query_label);
    }

    // Show relevant property based on entity type
    if let Some(personality) = query_node.properties.get("personality") {
        println!(
            "Personality: {}",
            wrap_text(&format_value(personality), 70, "             ")
        );
    } else if let Some(themes) = query_node.properties.get("themes") {
        println!(
            "Themes: {}",
            wrap_text(&format_value(themes), 70, "        ")
        );
    } else if let Some(style) = query_node.properties.get("writing_style") {
        println!("Style: {}", wrap_text(&format_value(style), 70, "       "));
    } else if let Some(desc) = query_node.properties.get("description") {
        println!(
            "Description: {}",
            wrap_text(&format_value(desc), 70, "             ")
        );
    }

    // Find similar entities using vector similarity
    let property_display = property_name.unwrap_or("semantic_embedding");
    if let Some(filter) = type_filter {
        println!(
            "\nFinding similar entities (type: {}, property: {})...",
            filter, property_display
        );
    } else {
        println!(
            "\nFinding similar entities (property: {})...",
            property_display
        );
    }

    // Validate that the specified property exists on the query node
    let property_to_use = property_name.unwrap_or("semantic_embedding");
    if !query_node.properties.contains_key(property_to_use) {
        println!(
            "\n❌ Error: Entity '{}' ({}) does not have property '{}'",
            query_label,
            label_str(query_node.label),
            property_to_use
        );

        // Show available vector properties
        let vector_props: Vec<String> = query_node
            .properties
            .iter()
            .filter(|(_, v)| matches!(v, aletheiadb::PropertyValue::Vector(_)))
            .map(|(k, _)| k.to_string())
            .collect();

        if !vector_props.is_empty() {
            println!("\nAvailable vector properties on this entity:");
            for prop in vector_props {
                println!("  • {}", prop);
            }
            println!(
                "\nTip: Try 'similar {} --in <property>' with one of the above",
                entity_name
            );
        } else {
            println!("\nThis entity has no vector embeddings.");
        }
        return Ok(());
    }

    // Use property-specific search if specified, otherwise use semantic_embedding as default
    let mut similar = timed!(demo, "Vector similarity search", {
        demo.db.find_similar_in(property_to_use, query_id, k * 3)?
    }); // Get more to allow filtering

    // Filter by type if specified
    if let Some(filter_type) = type_filter {
        similar.retain(|(node_id, _)| {
            if let Ok(node) = demo.db.get_node(*node_id) {
                label_str(node.label).eq_ignore_ascii_case(filter_type)
            } else {
                false
            }
        });
        similar.truncate(k);
    } else {
        similar.truncate(k);
    }

    if similar.is_empty() {
        println!("  No similar entities found (embeddings may be missing)");
    } else {
        println!("\nTop {} most similar entities:\n", similar.len());

        for (i, (similar_id, score)) in similar.iter().enumerate() {
            let node = demo.db.get_node(*similar_id)?;
            let entity_type = label_str(node.label);

            // Display based on entity type
            match entity_type.as_str() {
                "Character" => {
                    let name = node
                        .properties
                        .get("name")
                        .map(format_value)
                        .unwrap_or_default();
                    let book = node
                        .properties
                        .get("book")
                        .map(format_value)
                        .unwrap_or_default();
                    let personality = node
                        .properties
                        .get("personality")
                        .map(format_value)
                        .unwrap_or_default();

                    println!("{}. {} [Character] (similarity: {:.3})", i + 1, name, score);
                    println!("   from: {}", book);
                    println!("   {}", wrap_text(&personality, 60, "   "));
                    println!();
                }
                "Book" => {
                    let title = node
                        .properties
                        .get("title")
                        .map(format_value)
                        .unwrap_or_default();
                    let author = node
                        .properties
                        .get("author")
                        .map(format_value)
                        .unwrap_or_default();
                    let themes = node
                        .properties
                        .get("themes")
                        .map(format_value)
                        .unwrap_or_default();

                    println!("{}. {} [Book] (similarity: {:.3})", i + 1, title, score);
                    println!("   by: {}", author);
                    println!("   themes: {}", wrap_text(&themes, 60, "           "));
                    println!();
                }
                "Author" => {
                    let name = node
                        .properties
                        .get("name")
                        .map(format_value)
                        .unwrap_or_default();
                    let years = format!(
                        "{}-{}",
                        node.properties
                            .get("birth_year")
                            .map(format_value)
                            .unwrap_or_default(),
                        node.properties
                            .get("death_year")
                            .map(format_value)
                            .unwrap_or_default()
                    );
                    let style = node
                        .properties
                        .get("writing_style")
                        .map(format_value)
                        .unwrap_or_default();

                    println!("{}. {} [Author] (similarity: {:.3})", i + 1, name, score);
                    println!("   {}", years);
                    println!("   {}", wrap_text(&style, 60, "   "));
                    println!();
                }
                "Theme" => {
                    let name = node
                        .properties
                        .get("name")
                        .map(format_value)
                        .unwrap_or_default();
                    let description = node
                        .properties
                        .get("description")
                        .map(format_value)
                        .unwrap_or_default();

                    println!("{}. {} [Theme] (similarity: {:.3})", i + 1, name, score);
                    println!("   {}", wrap_text(&description, 60, "   "));
                    println!();
                }
                _ => {
                    println!("{}. [{}] (similarity: {:.3})", i + 1, entity_type, score);
                    println!();
                }
            }
        }
    }

    Ok(())
}

/// Query how a book's interpretation evolved over time using AletheiaDB's temporal API
///
/// This demonstrates bi-temporal queries by retrieving the book's state as it
/// existed at a specific point in time. The example shows how literary criticism
/// evolved from publication to present day.
fn timewarp_book(demo: &DemoData, book_title: &str, year: i64) -> Result<()> {
    if let Some((matched_name, &book_id)) = find_entity_fuzzy(&demo.books, book_title) {
        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!("║  TIME WARP: {} in {}", matched_name.to_uppercase(), year);
        println!("╚═══════════════════════════════════════════════════════════╝");

        // Get current state
        let current = demo.db.get_node(book_id)?;
        let current_interp = current
            .properties
            .get("interpretation")
            .map(format_value)
            .unwrap_or_default();

        println!("\nBook: {}", matched_name);
        println!(
            "Author: {}",
            current
                .properties
                .get("author")
                .map(format_value)
                .unwrap_or_default()
        );
        println!(
            "Published: {}",
            current
                .properties
                .get("published_year")
                .map(format_value)
                .unwrap_or_default()
        );

        println!("\n═══ TEMPORAL QUERY: INTERPRETATION IN {} ═══\n", year);

        // Convert year to timestamp and get current transaction time
        let query_timestamp = year_to_timestamp(year);
        let current_tx_time = now_timestamp()?;

        // Use AletheiaDB's temporal API to get the node as it was in that year
        match demo
            .db
            .get_node_at_time(book_id, query_timestamp, current_tx_time)
        {
            Ok(historical_node) => {
                let historical_interp = historical_node
                    .properties
                    .get("interpretation")
                    .map(format_value)
                    .unwrap_or_else(|| "No interpretation recorded yet".to_string());

                println!("Interpretation in {}:", year);
                println!("  {}\n", historical_interp);

                // Compare with current interpretation
                println!("Current interpretation (2024):");
                println!("  {}\n", current_interp);

                // Show temporal context
                if year < 1900 {
                    println!("In {}, literary criticism was still emerging.", year);
                    println!("The work would have been viewed through a 19th century lens.");
                } else if year < 1950 {
                    println!(
                        "In {}, modernist and early psychoanalytic interpretations",
                        year
                    );
                    println!("were beginning to influence literary criticism.");
                } else if year < 2000 {
                    println!(
                        "In {}, post-war critical theory and structural analysis",
                        year
                    );
                    println!("dominated literary interpretation.");
                } else {
                    println!("In {}, contemporary criticism emphasizes diverse", year);
                    println!("perspectives including trauma studies, postcolonial theory, etc.");
                }
            }
            Err(_) => {
                println!("⚠️  No version of this book exists at year {}", year);
                println!("The book may not have been published yet, or no updates were recorded.");
            }
        }

        println!("\n💡 This query used: db.get_node_at_time(book_id, valid_time, tx_time)");
        println!("   Valid Time: Jan 1, {} (approximate)", year);
        println!("   Transaction Time: now (latest committed state)");
    } else {
        println!("\n❌ Book not found: {}", book_title);
        println!("\nTry: list books");
    }

    Ok(())
}

fn show_influences(demo: &DemoData, author_name: &str) -> Result<()> {
    if let Some((matched_name, &author_id)) = find_entity_fuzzy(&demo.authors, author_name) {
        let author = demo.db.get_node(author_id)?;

        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!("║  INFLUENCE NETWORK: {}", matched_name.to_uppercase());
        println!("╚═══════════════════════════════════════════════════════════╝");

        let years = format!(
            "{}-{}",
            author
                .properties
                .get("birth_year")
                .map(format_value)
                .unwrap_or_default(),
            author
                .properties
                .get("death_year")
                .map(format_value)
                .unwrap_or_default()
        );
        println!("\n{} ({})", matched_name, years);

        // === 1. WHO INFLUENCED THIS AUTHOR (Graph Traversal) ===
        println!("\n═══ INFLUENCED BY (Graph Traversal) ═══");
        let influenced_by_nodes = timed!(demo, "Graph traversal (INFLUENCED_BY)", {
            let results = demo
                .db
                .query()
                .start(author_id)
                .traverse("INFLUENCED_BY")
                .execute(&demo.db)?;
            results.collect_nodes()
        })?;

        if influenced_by_nodes.is_empty() {
            println!("  (No recorded influences)");
        } else {
            for influencer in &influenced_by_nodes {
                let node_id = influencer.id;
                let influencer_name = get_entity_name(influencer)?;

                // Get the edge to find influence type
                for edge_id in demo.db.get_outgoing_edges(author_id) {
                    if let Ok(edge) = demo.db.get_edge(edge_id)
                        && edge.target == node_id
                        && label_str(edge.label) == "INFLUENCED_BY"
                    {
                        let influence_type = edge
                            .properties
                            .get("type")
                            .map(format_value)
                            .unwrap_or_else(|| "general influence".to_string());
                        println!("  ← {} ({})", influencer_name, influence_type);
                        break;
                    }
                }
            }
        }

        // === 2. STYLISTICALLY SIMILAR AUTHORS (Hybrid: Vector Ranking) ===
        println!("\n═══ STYLISTICALLY SIMILAR (Vector Similarity) ═══");

        // Get style embedding for this author
        if let Some(aletheiadb::PropertyValue::Vector(style_vec)) =
            author.properties.get("style_embedding")
        {
            let ranked_nodes = timed!(demo, "Hybrid query (scan + rank_by_similarity)", {
                let results = demo
                    .db
                    .query()
                    .scan_label("Author")
                    .rank_by_similarity_builder(style_vec, 6)
                    .property("style_embedding")
                    .finish()
                    .execute(&demo.db)?;
                results.collect_nodes_with_scores()
            })?;

            let mut shown = 0;
            for (similar_author, score) in &ranked_nodes {
                // Skip self
                if similar_author.id == author_id {
                    continue;
                }

                let similar_author_node = demo.db.get_node(similar_author.id)?;
                let name = get_entity_name(&similar_author_node)?;
                println!("  ≈ {} (similarity: {:.3})", name, score);

                shown += 1;
                if shown >= 5 {
                    break;
                }
            }
        } else {
            println!("  (No style embedding available)");
        }

        // === 3. WHO DID THIS AUTHOR INFLUENCE (Reverse Traversal) ===
        println!("\n═══ INFLUENCED (Reverse Graph Traversal) ═══");
        let influenced_results = demo
            .db
            .query()
            .start(author_id)
            .traverse_in("INFLUENCED_BY")
            .execute(&demo.db)?;

        let influenced_nodes = influenced_results.collect_nodes()?;

        if influenced_nodes.is_empty() {
            println!("  (No recorded literary descendants)");
        } else {
            for influenced in &influenced_nodes {
                let node_id = influenced.id;
                let influenced_name = get_entity_name(influenced)?;

                // Get the edge to find influence type
                for edge_id in demo.db.get_incoming_edges(author_id) {
                    if let Ok(edge) = demo.db.get_edge(edge_id)
                        && edge.source == node_id
                        && label_str(edge.label) == "INFLUENCED_BY"
                    {
                        let influence_type = edge
                            .properties
                            .get("type")
                            .map(format_value)
                            .unwrap_or_else(|| "general influence".to_string());
                        println!("  → {} ({})", influenced_name, influence_type);
                        break;
                    }
                }
            }
        }

        // === 4. MAJOR WORKS ===
        println!("\n═══ MAJOR WORKS ═══");
        let works_results = demo
            .db
            .query()
            .start(author_id)
            .traverse("WROTE")
            .execute(&demo.db)?;

        let works_nodes = works_results.collect_nodes()?;

        let mut works = vec![];
        for book in &works_nodes {
            let title = book
                .properties
                .get("title")
                .map(format_value)
                .unwrap_or_default();
            let year = book
                .properties
                .get("published_year")
                .map(format_value)
                .unwrap_or_default()
                .parse::<i64>()
                .unwrap_or(0);
            works.push((year, title));
        }
        works.sort_by_key(|(year, _)| *year);
        for (year, title) in works {
            println!("  • {} ({})", title, year);
        }

        println!("\n💡 This command demonstrates hybrid queries:");
        println!("   • Graph traversal (.traverse, .traverse_in)");
        println!("   • Vector ranking (.rank_by_similarity_builder)");
    } else {
        println!("\n❌ Author not found: {}", author_name);
        println!("\nTry: list authors");
    }

    Ok(())
}

fn find_semantic_path(
    demo: &DemoData,
    from_name: &str,
    to_name: &str,
    concept_name: &str,
) -> Result<()> {
    // Try to find entities using fuzzy matching across all entity types
    let from_entity = find_any_entity(demo, from_name);
    let to_entity = find_any_entity(demo, to_name);
    let concept_entity = find_any_entity(demo, concept_name);

    let (from_id, from_display) = match from_entity {
        Some((name, id, entity_type)) => (id, format!("{} ({})", name, entity_type)),
        None => {
            println!("\n❌ Source entity not found: {}", from_name);
            println!("\nTry: list authors, list books, list characters, or list themes");
            return Ok(());
        }
    };

    let (to_id, to_display) = match to_entity {
        Some((name, id, entity_type)) => (id, format!("{} ({})", name, entity_type)),
        None => {
            println!("\n❌ Target entity not found: {}", to_name);
            println!("\nTry: list authors, list books, list characters, or list themes");
            return Ok(());
        }
    };

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  SEMANTIC PATHFINDING");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!("\n🎯 Finding path from {} to {}", from_display, to_display);

    // Get concept embedding
    let concept_embedding = match concept_entity {
        Some((concept_display, concept_id, _)) => {
            println!("🧭 Guided by concept: {}\n", concept_display);
            // Try to get semantic_embedding from the concept entity
            let concept_node = demo.db.get_node(concept_id)?;
            if let Some(aletheiadb::PropertyValue::Vector(vec)) =
                concept_node.properties.get("semantic_embedding")
            {
                vec.clone()
            } else {
                println!(
                    "❌ No semantic_embedding found for concept: {}",
                    concept_display
                );
                return Ok(());
            }
        }
        None => {
            println!("❌ Concept entity not found: {}", concept_name);
            println!("\n💡 Tip: Use an existing theme, author, book, or character as the concept.");
            println!("   Try: list themes");
            return Ok(());
        }
    };

    // Create pathfinder and find path
    let pathfinder = SemanticPathfinder::new(&demo.db, "semantic_embedding");
    let max_depth = 10; // Allow up to 10 hops

    let path_result = timed!(
        demo,
        "Semantic pathfinding (bidirectional)",
        pathfinder.find_path(from_id, to_id, &concept_embedding, max_depth, true)
    )?;

    match path_result {
        Some(path) => {
            println!("✅ Path found ({} hops):\n", path.len() - 1);

            // Display the path with entity details
            for (i, &node_id) in path.iter().enumerate() {
                let node = demo.db.get_node(node_id)?;
                let name = get_entity_name(&node)?;
                let label = label_str(node.label);

                // Calculate semantic score (similarity to concept)
                let semantic_score = if let Some(aletheiadb::PropertyValue::Vector(vec)) =
                    node.properties.get("semantic_embedding")
                {
                    aletheiadb::core::vector::cosine_similarity(vec, &concept_embedding)
                        .unwrap_or(0.0)
                } else {
                    0.0
                };

                if i == 0 {
                    println!("  🟢 START: {} [{}]", name, label);
                } else if i == path.len() - 1 {
                    println!(
                        "  🎯 END:   {} [{}] (relevance: {:.3})",
                        name, label, semantic_score
                    );
                } else {
                    // Find connecting edge for display
                    let prev_node = path[i - 1];
                    let mut edge_label = "→".to_string();
                    for edge_id in demo.db.get_outgoing_edges(prev_node) {
                        if let Ok(edge) = demo.db.get_edge(edge_id)
                            && edge.target == node_id
                        {
                            edge_label = label_str(edge.label);
                            break;
                        }
                    }
                    println!(
                        "  ↓   {}   {} [{}] (relevance: {:.3})",
                        edge_label, name, label, semantic_score
                    );
                }
            }

            println!("\n💡 This path was chosen because each hop maximizes semantic similarity");
            println!("   to the concept, creating a meaningful thematic connection!");
        }
        None => {
            println!("❌ No path found within {} hops", max_depth);
            println!("\n💡 Try:");
            println!("   • Entities that are more closely connected in the graph");
            println!("   • A different concept that bridges them better");
        }
    }

    Ok(())
}

fn show_semantic_drift(demo: &DemoData, character_name: &str) -> Result<()> {
    // Find character with fuzzy matching
    if let Some((name, &character_id)) = find_character_fuzzy(&demo.characters, character_name) {
        println!("\n=== Semantic Drift: {} ===\n", name);

        // Get the original (current) embedding
        let current_node = demo.db.get_node(character_id)?;
        let reference_embedding = if let Some(aletheiadb::PropertyValue::Vector(vec)) =
            current_node.properties.get("semantic_embedding")
        {
            vec.clone()
        } else {
            println!("❌ No semantic_embedding found for this character");
            return Ok(());
        };

        // Track drift from 1866 to present
        use aletheiadb::core::temporal::TimeRange;
        let time_range = TimeRange::new(
            year_to_timestamp(1866), // Start of our data
            now_timestamp()?,        // Current time
        )?;

        println!("Tracking semantic drift from original understanding to present:\n");

        // Get drift timeline using the new property-based API
        match timed!(
            demo,
            "Temporal vector drift tracking",
            demo.db.track_drift_in(
                "semantic_embedding",
                character_id,
                &reference_embedding,
                time_range,
            )
        ) {
            Ok(drift_vec) => {
                if drift_vec.is_empty() {
                    println!(
                        "  No temporal versions found (character may not have evolving embeddings)"
                    );
                } else {
                    println!("  Time Point              Cosine Distance  Interpretation");
                    println!("  ───────────────────────────────────────────────────────────");

                    // Filter consecutive duplicates to show only meaningful drift changes
                    let mut prev_distance: Option<f32> = None;
                    let mut shown_count = 0;
                    const MAX_DRIFT_ENTRIES: usize = 10;

                    for (timestamp, similarity) in &drift_vec {
                        // Convert similarity to distance: distance = 1.0 - similarity
                        let distance = 1.0 - similarity;

                        // Skip consecutive duplicates (same distance within 0.001 threshold)
                        if let Some(prev) = prev_distance
                            && (distance - prev).abs() < 0.001
                        {
                            continue;
                        }

                        // Limit output to avoid flooding console
                        if shown_count >= MAX_DRIFT_ENTRIES {
                            println!(
                                "  ... ({} more snapshots omitted)",
                                drift_vec.len() - shown_count
                            );
                            break;
                        }

                        // Get approximate year from timestamp
                        let year_approx = 2024; // Simplified - actual conversion would use timestamp

                        // Try to get personality at this point in time
                        let personality_short = if let Ok(historical_node) = demo
                            .db
                            .get_node_at_time(character_id, *timestamp, now_timestamp()?)
                        {
                            if let Some(personality) = historical_node.properties.get("personality")
                            {
                                let personality_text = format_value(personality);
                                if personality_text.chars().count() > 50 {
                                    let truncated: String =
                                        personality_text.chars().take(47).collect();
                                    format!("{}...", truncated)
                                } else {
                                    personality_text
                                }
                            } else {
                                "(no personality data)".to_string()
                            }
                        } else {
                            "(version not available)".to_string()
                        };

                        println!(
                            "  ~{:4}                  {:.4}           {}",
                            year_approx, distance, personality_short
                        );

                        prev_distance = Some(distance);
                        shown_count += 1;
                    }

                    if shown_count == 0 {
                        println!(
                            "  (All drift values were duplicates - no semantic evolution detected)"
                        );
                    }

                    println!(
                        "\n  💡 Cosine distance measures semantic drift (0.0 = identical, 2.0 = opposite)"
                    );
                    println!(
                        "     Higher values indicate our understanding of the character has evolved"
                    );
                }
            }
            Err(e) => {
                println!("  ⚠️  Error tracking drift: {}", e);
                println!(
                    "\n  💡 Note: Character embeddings don't have temporal evolution in this demo."
                );
                println!(
                    "     The drift feature works, but requires embeddings to be updated over time."
                );
                println!("\n  Try these working features instead:");
                println!("     • influences Tolstoy  - hybrid graph+vector queries");
                println!(
                    "     • timewarp \"Crime and Punishment\" 1900  - time-travel queries for books"
                );
            }
        }
    } else {
        println!("\n❌ Character not found: {}", character_name);
        println!("\nTry: list characters");
    }

    Ok(())
}

#[allow(dead_code)]
fn show_personality_evolution(demo: &DemoData, book_title: &str) -> Result<()> {
    // Find book with fuzzy matching
    if let Some((matched_name, &book_id)) = find_entity_fuzzy(&demo.books, book_title) {
        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!(
            "║  LITERARY CRITICISM EVOLUTION: {}",
            matched_name.to_uppercase()
        );
        println!("╚═══════════════════════════════════════════════════════════╝");

        let current_time = now_timestamp()?;

        println!("\nHow our interpretation of this work evolved from publication to present:\n");

        // Query interpretation at different time periods
        // We know books were updated at: 1885, 1900, 1925, 1945, 1955, 1960, 2024
        let years = vec![1885, 1900, 1925, 1945, 1955, 1960, 2024];

        let mut found_any = false;
        for year in years {
            let timestamp = year_to_timestamp(year as i64);

            // Query book state at this time
            match timed!(
                demo,
                &format!("Temporal query (year {})", year),
                demo.db.get_node_at_time(book_id, timestamp, current_time)
            ) {
                Ok(historical_node) => {
                    // Debug: show available properties
                    if !found_any {
                        println!("\n  Debug: Available properties in node:");
                        for (key, _value) in historical_node.properties.iter() {
                            println!("    • {}", label_str(*key));
                        }
                    }

                    if let Some(interpretation) = historical_node.properties.get("interpretation") {
                        let interp_text = format_value(interpretation);

                        println!("┌─ {} {}", year, "─".repeat(60 - year.to_string().len()));
                        println!("│");
                        let wrapped = wrap_text(&interp_text, 68, "│ ");
                        println!("{}", wrapped);
                        println!("│");

                        found_any = true;
                    } else {
                        println!(
                            "  Debug: Year {} - no 'interpretation' property found",
                            year
                        );
                    }
                }
                Err(e) => {
                    println!("  Debug: Year {} - query error: {}", year, e);
                    continue;
                }
            }
        }

        if found_any {
            println!("└{}\n", "─".repeat(65));
            println!("💡 This shows how literary criticism evolved from publication to present");
            println!("   Each version represents the scholarly interpretation at that time");
        } else {
            println!("  No temporal versions found for this book");
            println!("\n  💡 Note: Only some books have interpretation evolution data.");
            println!("     Try: evolution \"Crime and Punishment\"");
            println!("          evolution \"Anna Karenina\"");
        }
    } else {
        println!("\n❌ Book not found: {}", book_title);
        println!("\nTry: list books");
    }

    Ok(())
}

/// Find authors with similar writing styles using style_embedding
fn show_style_similarity(demo: &DemoData, author_name: &str, k: usize) -> Result<()> {
    if let Some((matched_name, &author_id)) = find_entity_fuzzy(&demo.authors, author_name) {
        let author = demo.db.get_node(author_id)?;

        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!("║  STYLISTIC SIMILARITY: {}", matched_name.to_uppercase());
        println!("╚═══════════════════════════════════════════════════════════╝");

        // Show query author's writing style
        if let Some(writing_style) = author.properties.get("writing_style") {
            println!(
                "\nWriting style: {}",
                wrap_text(&format_value(writing_style), 70, "               ")
            );
        }

        println!("\nAuthors with similar writing styles:\n");

        // Find similar authors using style_embedding
        if let Some(aletheiadb::PropertyValue::Vector(_)) = author.properties.get("style_embedding")
        {
            let similar = timed!(
                demo,
                "Vector search (style_embedding)",
                demo.db.find_similar_in("style_embedding", author_id, k + 1)
            )?;

            if similar.is_empty() || (similar.len() == 1 && similar[0].0 == author_id) {
                println!("  No similar authors found (embeddings may be missing)");
            } else {
                let mut count = 0;
                for (similar_id, score) in similar.iter() {
                    // Skip self
                    if *similar_id == author_id {
                        continue;
                    }

                    let similar_author = demo.db.get_node(*similar_id)?;
                    let name = get_entity_name(&similar_author)?;
                    let years = format!(
                        "{}-{}",
                        similar_author
                            .properties
                            .get("birth_year")
                            .map(format_value)
                            .unwrap_or_default(),
                        similar_author
                            .properties
                            .get("death_year")
                            .map(format_value)
                            .unwrap_or_default()
                    );

                    println!(
                        "{}. {} ({}) - Similarity: {:.3}",
                        count + 1,
                        name,
                        years,
                        score
                    );

                    if let Some(style) = similar_author.properties.get("writing_style") {
                        let style_str = format_value(style);
                        let truncated: String = style_str.chars().take(70).collect();
                        let suffix = if style_str.chars().count() > 70 {
                            "..."
                        } else {
                            ""
                        };
                        println!("   {}{}", truncated, suffix);
                    }
                    println!();

                    count += 1;
                    if count >= k {
                        break;
                    }
                }
            }
        } else {
            println!("  No style_embedding available for this author");
        }

        println!("💡 Using vector index: style_embedding (writing style analysis)");
    } else {
        println!("\n❌ Author not found: {}", author_name);
        println!("\nTry: list authors");
    }

    Ok(())
}

/// Find characters with similar personality archetypes using personality_embedding
fn show_character_archetypes(demo: &DemoData, character_name: &str, k: usize) -> Result<()> {
    if let Some((name, &character_id)) = find_character_fuzzy(&demo.characters, character_name) {
        let character = demo.db.get_node(character_id)?;

        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!("║  CHARACTER ARCHETYPE: {}", name.to_uppercase());
        println!("╚═══════════════════════════════════════════════════════════╝");

        // Show query character's personality
        println!("\nCharacter: {}", name);
        if let Some(book) = character.properties.get("book") {
            println!("From: {}", format_value(book));
        }
        if let Some(personality) = character.properties.get("personality") {
            println!(
                "\nPersonality:\n  {}",
                wrap_text(&format_value(personality), 70, "  ")
            );
        }

        println!("\n\nSimilar character archetypes:\n");

        // Find similar characters using personality_embedding
        if let Some(aletheiadb::PropertyValue::Vector(_)) =
            character.properties.get("personality_embedding")
        {
            let similar = timed!(
                demo,
                "Vector search (personality_embedding)",
                demo.db
                    .find_similar_in("personality_embedding", character_id, k + 1)
            )?;

            if similar.is_empty() || (similar.len() == 1 && similar[0].0 == character_id) {
                println!("  No similar characters found (embeddings may be missing)");
            } else {
                let mut count = 0;
                for (similar_id, score) in similar.iter() {
                    // Skip self
                    if *similar_id == character_id {
                        continue;
                    }

                    let similar_char = demo.db.get_node(*similar_id)?;
                    let char_name = get_entity_name(&similar_char)?;
                    let book = similar_char
                        .properties
                        .get("book")
                        .map(format_value)
                        .unwrap_or_default();

                    println!("{}. {} - Similarity: {:.3}", count + 1, char_name, score);
                    println!("   from: {}", book);

                    if let Some(personality) = similar_char.properties.get("personality") {
                        let personality_str = format_value(personality);
                        let truncated: String = personality_str.chars().take(70).collect();
                        let suffix = if personality_str.chars().count() > 70 {
                            "..."
                        } else {
                            ""
                        };
                        println!("   {}{}", truncated, suffix);
                    }
                    println!();

                    count += 1;
                    if count >= k {
                        break;
                    }
                }
            }
        } else {
            println!("  No personality_embedding available for this character");
        }

        println!("💡 Using vector index: personality_embedding (character archetype analysis)");
    } else {
        println!("\n❌ Character not found: {}", character_name);
        println!("\nTry: list characters");
    }

    Ok(())
}

/// Find books/themes with similar thematic content using theme_embedding
fn show_thematic_similarity(demo: &DemoData, entity_name: &str, k: usize) -> Result<()> {
    // Try to find entity in books or themes using fuzzy matching
    let (query_id, entity_type, matched_name) = find_entity_fuzzy(&demo.books, entity_name)
        .map(|(name, id)| (*id, "Book", name.clone()))
        .or_else(|| {
            find_entity_fuzzy(&demo.themes, entity_name)
                .map(|(name, id)| (*id, "Theme", name.clone()))
        })
        .ok_or_else(|| {
            aletheiadb::Error::other(format!("Book or theme not found: {}", entity_name))
        })?;

    let query_entity = demo.db.get_node(query_id)?;

    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  THEMATIC SIMILARITY: {}", matched_name.to_uppercase());
    println!("╚═══════════════════════════════════════════════════════════╝");

    println!("\nQuery entity: {} ({})", matched_name, entity_type);

    // Show themes or description
    if let Some(themes) = query_entity.properties.get("themes") {
        println!(
            "Themes: {}",
            wrap_text(&format_value(themes), 70, "        ")
        );
    } else if let Some(description) = query_entity.properties.get("description") {
        println!(
            "Description: {}",
            wrap_text(&format_value(description), 70, "             ")
        );
    }

    println!("\n\nThematically similar entities:\n");

    // Find similar entities using theme_embedding
    if let Some(aletheiadb::PropertyValue::Vector(_)) =
        query_entity.properties.get("theme_embedding")
    {
        let similar = timed!(
            demo,
            "Vector search (theme_embedding)",
            demo.db.find_similar_in("theme_embedding", query_id, k + 1)
        )?;

        if similar.is_empty() || (similar.len() == 1 && similar[0].0 == query_id) {
            println!("  No similar entities found (embeddings may be missing)");
        } else {
            let mut count = 0;
            for (similar_id, score) in similar.iter() {
                // Skip self
                if *similar_id == query_id {
                    continue;
                }

                let similar_entity = demo.db.get_node(*similar_id)?;
                let name = get_entity_name(&similar_entity)?;
                let label = label_str(similar_entity.label);

                println!(
                    "{}. {} ({}) - Similarity: {:.3}",
                    count + 1,
                    name,
                    label,
                    score
                );

                // Show relevant property
                if label == "Book"
                    && let Some(themes) = similar_entity.properties.get("themes")
                {
                    let themes_str = format_value(themes);
                    let truncated: String = themes_str.chars().take(70).collect();
                    let suffix = if themes_str.chars().count() > 70 {
                        "..."
                    } else {
                        ""
                    };
                    println!("   {}{}", truncated, suffix);
                } else if label == "Theme"
                    && let Some(description) = similar_entity.properties.get("description")
                {
                    let desc_str = format_value(description);
                    let truncated: String = desc_str.chars().take(70).collect();
                    let suffix = if desc_str.chars().count() > 70 {
                        "..."
                    } else {
                        ""
                    };
                    println!("   {}{}", truncated, suffix);
                }
                println!();

                count += 1;
                if count >= k {
                    break;
                }
            }
        }
    } else {
        println!("  No theme_embedding available for this entity");
    }

    println!("💡 Using vector index: theme_embedding (thematic content analysis)");

    Ok(())
}

/// Show all active vector indexes and their statistics
fn show_vector_indexes(demo: &DemoData) -> Result<()> {
    println!("\n╔═══════════════════════════════════════════════════════════╗");
    println!("║  VECTOR INDEXES");
    println!("╚═══════════════════════════════════════════════════════════╝");

    let indexes = demo.db.list_vector_indexes();

    if indexes.is_empty() {
        println!("\nNo vector indexes enabled.");
    } else {
        println!("\n{} active vector index(es):\n", indexes.len());

        for (i, idx) in indexes.iter().enumerate() {
            println!("{}. Property: {}", i + 1, idx.property_name);
            println!("   Dimensions: {}", idx.dimensions);
            println!("   Distance metric: {:?}", idx.distance_metric);

            // Show example usage based on property
            match idx.property_name.as_str() {
                "semantic_embedding" => {
                    println!("   Use: Cross-entity similarity search");
                    println!("   Example: similar Raskolnikov");
                }
                "style_embedding" => {
                    println!("   Use: Author writing style comparison");
                    println!("   Example: styles Dostoevsky");
                }
                "theme_embedding" => {
                    println!("   Use: Thematic content similarity");
                    println!("   Example: thematic \"Crime and Punishment\"");
                }
                "personality_embedding" => {
                    println!("   Use: Character archetype matching");
                    println!("   Example: archetypes Raskolnikov");
                }
                _ => {
                    println!("   Use: Property-specific similarity");
                    println!("   Example: similar <entity> --in {}", idx.property_name);
                }
            }
            println!();
        }
    }

    Ok(())
}

fn print_help() {
    println!(
        r#"
╔═══════════════════════════════════════════════════════════╗
║  RUSSIAN WRITERS KNOWLEDGE GRAPH - COMMANDS
╚═══════════════════════════════════════════════════════════╝

BROWSE:
  list authors             - List all authors
  list books               - List all books
  list characters          - List all characters
  list themes              - List all themes
  show <name>              - Show details for an entity

SEMANTIC SEARCH (Multi-Property Vector Indexes):
  similar <entity> [--in <property>] [--type <Type>]
                           - Find similar entities (cross-entity or property-specific)
  styles <author> [k]      - Find authors with similar writing styles
  archetypes <character> [k] - Find similar character types
  thematic <book/theme> [k] - Find thematic similarities
  drift <character>        - Show semantic drift over time
  indexes                  - Show all active vector indexes

TEMPORAL QUERIES (Time Travel):
  timewarp <book> <year>   - See how interpretation evolved
  evolution <character>    - Show personality evolution timeline

GRAPH QUERIES (Query Builder API):
  influences <author>      - Show influences (graph + vector hybrid)
  path <from> <to> --like <concept>
                           - Find semantic path between entities

SYSTEM:
  stats                    - Database statistics
  timing                   - Toggle performance timing on/off
  help                     - Show this help
  quit / exit              - Exit demo

Examples:
  > show Fyodor Dostoevsky
  > similar Raskolnikov
  > similar Dostoevsky --in style_embedding
  > styles Tolstoy
  > archetypes Raskolnikov 8
  > thematic "Crime and Punishment"
  > influences Dostoevsky
  > path Pushkin Gorky --like "Social Justice"
  > path Gogol Chekhov --like "Psychological Realism"
  > indexes
  > list books
"#
    );
}

// ============================================================================
// Main REPL
// ============================================================================

fn main() -> Result<()> {
    println!(
        r#"
╔═══════════════════════════════════════════════════════════╗
║                                                           ║
║   RUSSIAN WRITERS KNOWLEDGE GRAPH                         ║
║   AletheiaDB Comprehensive Demo                          ║
║                                                           ║
║   Showcasing:                                             ║
║   • Bi-temporal storage with evolving interpretations     ║
║   • Vector embeddings for semantic search                 ║
║   • Hybrid graph + vector queries                         ║
║   • Rich relational data from Russian literature          ║
║                                                           ║
╚═══════════════════════════════════════════════════════════╝
"#
    );

    // Check data files exist
    if let Err(e) = check_data_files() {
        eprintln!("\n❌ Error: {}", e);
        return Err(e);
    }

    let mut demo = DemoData::new();

    // Load and populate database
    populate_database(&mut demo)?;

    // Create temporal versions
    create_temporal_versions(&mut demo)?;

    print_help();

    // Initialize rustyline with auto-complete
    let completer = RussianLitCompleter::new(&demo);
    let mut rl = Editor::new()
        .map_err(|e| aletheiadb::Error::other(format!("Failed to initialize readline: {}", e)))?;
    rl.set_helper(Some(completer));

    println!("\n💡 Tip: Use TAB for auto-complete!");

    // Main REPL loop
    loop {
        let readline = rl.readline("\nrussian-lit> ");
        let input = match readline {
            Ok(line) => {
                rl.add_history_entry(&line).map_err(|e| {
                    aletheiadb::Error::other(format!("Failed to add history entry: {}", e))
                })?;
                line
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl-C
                continue;
            }
            Err(ReadlineError::Eof) => {
                // Ctrl-D
                println!("\n До свидания! (Goodbye!)\n");
                break;
            }
            Err(_) => {
                break;
            }
        };

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        let command = parts[0].to_lowercase();
        let args = parts.get(1).map(|s| s.trim()).unwrap_or("");

        match command.as_str() {
            "quit" | "exit" | "q" => {
                println!("\n До свидания! (Goodbye!)\n");
                break;
            }
            "help" | "h" | "?" => print_help(),
            "list" | "ls" => {
                if args.is_empty() {
                    println!("Usage: list <type>");
                    println!("Types: authors, books, characters, themes");
                } else {
                    list_entities(&demo, args)?;
                }
            }
            "show" | "s" => {
                if args.is_empty() {
                    println!("Usage: show <entity_name>");
                    println!("Example: show Fyodor Dostoevsky");
                } else {
                    show_entity(&demo, args)?;
                }
            }
            "stats" => show_stats(&demo)?,
            "timing" => {
                let was_enabled = demo
                    .timing_enabled
                    .fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
                let now_enabled = !was_enabled;
                if now_enabled {
                    println!("✓ Performance timing enabled");
                    println!("  All operations will show execution time");
                } else {
                    println!("✓ Performance timing disabled");
                }
            }
            "similar" | "sim" => {
                if args.is_empty() {
                    println!("Usage: similar <entity_name> [--type <Type>] [--in <property>]");
                    println!("Example: similar Raskolnikov");
                    println!("         similar Dmitri --type Book");
                    println!("         similar Dostoevsky --in style_embedding");
                    println!("\nTypes: Character, Book, Author, Theme");
                    println!(
                        "Properties: semantic_embedding, style_embedding, theme_embedding, personality_embedding"
                    );
                } else {
                    // Parse arguments: entity name and optional --type and --in filters
                    let parts: Vec<&str> = args.split_whitespace().collect();
                    let mut entity_name = String::new();
                    let mut type_filter = None;
                    let mut property_name = None;

                    let mut i = 0;
                    while i < parts.len() {
                        if parts[i] == "--type" && i + 1 < parts.len() {
                            type_filter = Some(parts[i + 1]);
                            i += 2;
                        } else if parts[i] == "--in" && i + 1 < parts.len() {
                            property_name = Some(parts[i + 1]);
                            i += 2;
                        } else {
                            if !entity_name.is_empty() {
                                entity_name.push(' ');
                            }
                            entity_name.push_str(parts[i]);
                            i += 1;
                        }
                    }

                    find_similar_entities(&demo, &entity_name, 5, type_filter, property_name)?;
                }
            }
            "timewarp" | "tw" => {
                let parts = parse_quoted_args(args);
                if parts.len() < 2 {
                    println!("Usage: timewarp <book_title> <year>");
                    println!("Example: timewarp \"Crime and Punishment\" 1900");
                    println!("\nTry: list books");
                } else if let Ok(year) = parts[1].parse::<i64>() {
                    timewarp_book(&demo, &parts[0], year)?;
                } else {
                    println!("Invalid year: {}", parts[1]);
                }
            }
            "influences" | "inf" => {
                if args.is_empty() {
                    println!("Usage: influences <author_name>");
                    println!("Example: influences Dostoevsky");
                    println!("         influences Tolstoy");
                    println!("\nTry: list authors");
                } else {
                    show_influences(&demo, strip_quotes(args))?;
                }
            }
            "drift" => {
                if args.is_empty() {
                    println!("Usage: drift <character_name>");
                    println!("Example: drift Raskolnikov");
                    println!(
                        "\nShows semantic drift over time (requires temporal embedding updates)"
                    );
                    println!(
                        "Note: This demo doesn't include temporal evolution for character embeddings."
                    );
                    println!("\nFor working features, try:");
                    println!("  • influences Tolstoy  - hybrid graph+vector queries");
                    println!("  • similar Dostoevsky  - cross-entity similarity");
                } else {
                    show_semantic_drift(&demo, strip_quotes(args))?;
                }
            }
            "evolution" | "evo" => {
                println!("\n❌ The 'evolution' command is currently disabled.");
                println!(
                    "\n💡 Why: This command requires setting explicit valid times when updating nodes,"
                );
                println!("   but AletheiaDB's current API doesn't support backdating valid times.");
                println!("\n📝 Technical details:");
                println!("   - To show interpretation evolution from 1885→1925→1955→2024,");
                println!(
                    "   - We need: update_node_with_valid_time(node_id, props, year_1885_timestamp)"
                );
                println!("   - Currently: BiTemporalInterval::current() sets valid_time = now()");
                println!("\n✨ This is a great feature request for AletheiaDB's bi-temporal API!");
                println!("\nTry these working features instead:");
                println!("  • similar Dostoevsky    - cross-entity semantic similarity");
                println!("  • influences Tolstoy    - hybrid graph+vector queries");
                println!("  • styles Dostoevsky     - writing style similarity");
                println!("  • archetypes Raskolnikov - character archetype similarity");
            }
            "styles" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.is_empty() {
                    println!("Usage: styles <author_name> [k]");
                    println!("Example: styles Dostoevsky");
                    println!("         styles Tolstoy 8");
                    println!("\nFinds authors with similar writing styles");
                    println!("Try: list authors");
                } else {
                    let author_name = parts[0];
                    let k = if parts.len() > 1 {
                        parts[1].parse::<usize>().unwrap_or(5)
                    } else {
                        5
                    };
                    show_style_similarity(&demo, author_name, k)?;
                }
            }
            "archetypes" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.is_empty() {
                    println!("Usage: archetypes <character_name> [k]");
                    println!("Example: archetypes Raskolnikov");
                    println!("         archetypes \"Anna Karenina\" 8");
                    println!("\nFinds characters with similar personality archetypes");
                    println!("Try: list characters");
                } else {
                    let character_name = parts[0];
                    let k = if parts.len() > 1 {
                        parts[1].parse::<usize>().unwrap_or(5)
                    } else {
                        5
                    };
                    show_character_archetypes(&demo, character_name, k)?;
                }
            }
            "thematic" => {
                let parts = parse_quoted_args(args);
                if parts.is_empty() {
                    println!("Usage: thematic <book_or_theme> [k]");
                    println!("Example: thematic \"Crime and Punishment\"");
                    println!("         thematic Nihilism 8");
                    println!("\nFinds books/themes with similar thematic content");
                    println!("Try: list books, list themes");
                } else {
                    let entity_name = &parts[0];
                    let k = if parts.len() > 1 {
                        parts[1].parse::<usize>().unwrap_or(5)
                    } else {
                        5
                    };
                    show_thematic_similarity(&demo, entity_name, k)?;
                }
            }
            "indexes" => {
                show_vector_indexes(&demo)?;
            }
            "path" => {
                let parts = parse_quoted_args(args);
                if parts.len() < 3 {
                    println!("Usage: path <from> <to> --like <concept>");
                    println!("Example: path Pushkin Gorky --like \"Social Justice\"");
                    println!("         path Gogol Chekhov --like \"Psychological Realism\"");
                    println!("         path Turgenev Dostoevsky --like Nihilism");
                    println!(
                        "\n💡 The concept guides the pathfinding to prefer semantically relevant hops."
                    );
                    println!(
                        "   Use any theme, author, book, or character as the guiding concept."
                    );
                } else {
                    // Parse: path <from> <to> --like <concept>
                    let from_name = &parts[0];
                    let to_name = &parts[1];

                    // Find --like flag
                    let mut concept_name = None;
                    for i in 2..parts.len() {
                        if parts[i] == "--like" && i + 1 < parts.len() {
                            concept_name = Some(parts[i + 1].as_str());
                            break;
                        }
                    }

                    if let Some(concept) = concept_name {
                        find_semantic_path(&demo, from_name, to_name, concept)?;
                    } else {
                        println!("❌ Missing --like <concept> parameter");
                        println!("Example: path Pushkin Gorky --like \"Social Justice\"");
                    }
                }
            }
            _ => {
                println!(
                    "Unknown command: {}. Type 'help' for available commands.",
                    command
                );
            }
        }
    }

    Ok(())
}
