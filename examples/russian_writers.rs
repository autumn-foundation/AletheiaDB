//! Russian Writers Knowledge Graph - Comprehensive GallifreyDB Demo
//!
//! This example demonstrates GallifreyDB's capabilities using Russian literary history:
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

use gallifreydb::{
    GLOBAL_INTERNER, GallifreyDB, InternedString, NodeId, PropertyMapBuilder, Result, Timestamp,
    WriteOps,
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
use std::time::{SystemTime, UNIX_EPOCH};

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
}

#[derive(Debug, Deserialize, Serialize)]
struct Theme {
    name: String,
    description: String,
    examples: String,
    #[serde(default)]
    theme_embedding: Option<Vec<f32>>,
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
                "timewarp".to_string(),
                "tw".to_string(),
                "influences".to_string(),
                "inf".to_string(),
                "drift".to_string(),
                "evolution".to_string(),
                "evo".to_string(),
                "list authors".to_string(),
                "list books".to_string(),
                "list characters".to_string(),
                "list themes".to_string(),
                "stats".to_string(),
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
                "drift" | "evolution" | "evo" => {
                    for name in &self.characters {
                        if name.to_lowercase().contains(&arg_prefix) {
                            candidates.push(Pair {
                                display: name.clone(),
                                replacement: name.clone(),
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
    db: GallifreyDB,
    // Track entities by type for easy lookup
    authors: HashMap<String, NodeId>,
    books: HashMap<String, NodeId>,
    characters: HashMap<String, NodeId>,
    themes: HashMap<String, NodeId>,
}

impl DemoData {
    fn new() -> Self {
        Self {
            db: GallifreyDB::new(),
            authors: HashMap::new(),
            books: HashMap::new(),
            characters: HashMap::new(),
            themes: HashMap::new(),
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
        .resolve(label)
        .map(|s| s.to_string())
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

/// Get current timestamp in microseconds since UNIX epoch
fn now_timestamp() -> Result<Timestamp> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as Timestamp)
        .map_err(|e| gallifreydb::Error::other(format!("System time error: {}", e)))
}

/// Create approximate timestamp for Jan 1 of a given year
///
/// NOTE: This is an approximation that doesn't account for leap years.
/// It's sufficient for demonstration purposes but shouldn't be used
/// for production temporal queries. Use a proper datetime library like
/// `chrono` for accurate timestamp conversion.
fn year_to_timestamp(year: i64) -> Timestamp {
    // Rough approximation: microseconds since epoch for Jan 1 of that year
    // Average 365.25 days/year to approximate leap years
    let years_since_1970 = year.saturating_sub(1970);
    let days = (years_since_1970 * 365) + (years_since_1970 / 4);
    days.saturating_mul(86400).saturating_mul(1_000_000)
}

/// Helper to format property values nicely
fn format_value(value: &gallifreydb::PropertyValue) -> String {
    use gallifreydb::PropertyValue;
    match value {
        PropertyValue::Null => "null".to_string(),
        PropertyValue::Bool(b) => b.to_string(),
        PropertyValue::Int(i) => i.to_string(),
        PropertyValue::Float(f) => f.to_string(),
        PropertyValue::String(s) => s.to_string(),
        PropertyValue::Bytes(b) => format!("<{} bytes>", b.len()),
        PropertyValue::Array(arr) => format!("[{} items]", arr.len()),
        PropertyValue::Vector(v) => format!("<vector dim={}>", v.len()),
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
    let content = fs::read_to_string(path).map_err(gallifreydb::Error::Io)?;

    serde_json::from_str(&content)
        .map_err(|e| gallifreydb::Error::other(format!("JSON deserialization failed: {}", e)))
}

fn check_data_files() -> Result<()> {
    let data_dir = Path::new("examples/russian_writers/data");

    if !data_dir.exists() {
        return Err(gallifreydb::Error::Io(std::io::Error::new(
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
            return Err(gallifreydb::Error::Io(std::io::Error::new(
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

    // === ENABLE TEMPORAL VECTOR INDEXING ===
    // IMPORTANT: Must enable BEFORE creating nodes so embeddings are automatically indexed
    // Using temporal indexing to track semantic drift over time
    println!("\n  Setting up temporal vector indexes...");
    use gallifreydb::index::vector::temporal::TemporalVectorConfig;
    use gallifreydb::index::vector::{DistanceMetric, HnswConfig};

    let hnsw_config = HnswConfig::new(384, DistanceMetric::Cosine);
    let temporal_config = TemporalVectorConfig::default_with_hnsw(hnsw_config);

    demo.db
        .enable_temporal_vector_index("personality_embedding", temporal_config)?;
    println!("    ✓ Temporal vector index enabled for personality_embedding");
    println!("    ✓ Will create snapshots to track semantic drift");

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

        // Add embedding if present
        if let Some(embedding) = author.style_embedding {
            builder = builder.insert_vector("style_embedding", &embedding);
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

        let node_id = demo.db.create_node("Book", builder.build())?;
        demo.books.insert(book.title, node_id);
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
                demo.db.write(|tx| {
                    tx.update_node(book_id, props! { "interpretation" => *interpretation })
                })?;

                println!(
                    "    {} → {}",
                    year,
                    &interpretation[..60.min(interpretation.len())]
                );

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
            let original_embedding = if let Some(gallifreydb::PropertyValue::Vector(vec)) =
                original_node.properties.get("personality_embedding")
            {
                vec.clone()
            } else {
                println!("    ⚠️  No personality_embedding found, skipping");
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

                // Update both personality text AND embedding
                demo.db.write(|tx| {
                    tx.update_node(
                        character_id,
                        PropertyMapBuilder::new()
                            .insert("personality", *evolved_personality)
                            .insert_vector("personality_embedding", &perturbed_embedding)
                            .build(),
                    )
                })?;

                println!(
                    "    {} → {}",
                    year,
                    &evolved_personality[..60.min(evolved_personality.len())]
                );

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
                .resolve(**k)
                .map(|s| s.to_string())
                .unwrap_or_default()
        });

        for (key, value) in props {
            let key_str = GLOBAL_INTERNER
                .resolve(*key)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{:?}", key));

            let value_str = format_value(value);

            // Truncate long values
            if value_str.len() > 100 {
                println!("  {}: {}...", key_str, &value_str[..97]);
            } else {
                println!("  {}: {}", key_str, value_str);
            }
        }

        // Show relationships
        let outgoing = demo.db.get_outgoing_edges(node_id);
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

fn find_similar_characters(demo: &DemoData, character_name: &str, k: usize) -> Result<()> {
    // Find the character node with fuzzy matching
    if let Some((full_name, &char_id)) = find_character_fuzzy(&demo.characters, character_name) {
        let character = demo.db.get_node(char_id)?;

        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!("║  SEMANTIC SIMILARITY: {}", full_name.to_uppercase());
        println!("╚═══════════════════════════════════════════════════════════╝");

        // Get character details
        let personality = character
            .properties
            .get("personality")
            .map(format_value)
            .unwrap_or_default();
        println!("\nQuery character: {}", full_name);
        if character_name.to_lowercase() != full_name.to_lowercase() {
            println!("  (matched '{}' to '{}')", character_name, full_name);
        }
        println!(
            "Personality: {}",
            wrap_text(&personality, 70, "             ")
        );

        // Find similar characters using vector similarity
        println!("\nFinding similar characters...");
        let similar = demo.db.find_similar(char_id, k)?;

        if similar.is_empty() {
            println!("  No similar characters found (embeddings may be missing)");
        } else {
            println!("\nTop {} most similar characters:\n", similar.len());

            for (i, (similar_id, score)) in similar.iter().enumerate() {
                let similar_char = demo.db.get_node(*similar_id)?;
                let name = similar_char
                    .properties
                    .get("name")
                    .map(format_value)
                    .unwrap_or_default();
                let book = similar_char
                    .properties
                    .get("book")
                    .map(format_value)
                    .unwrap_or_default();
                let personality = similar_char
                    .properties
                    .get("personality")
                    .map(format_value)
                    .unwrap_or_default();

                println!("{}. {} (similarity: {:.3})", i + 1, name, score);
                println!("   from: {}", book);
                println!("   personality:");
                println!("      {}", wrap_text(&personality, 60, "      "));
                println!();
            }
        }
    } else {
        println!("\n❌ Character not found: {}", character_name);
        println!("\nTry: list characters");
    }

    Ok(())
}

/// Query how a book's interpretation evolved over time using GallifreyDB's temporal API
///
/// This demonstrates bi-temporal queries by retrieving the book's state as it
/// existed at a specific point in time. The example shows how literary criticism
/// evolved from publication to present day.
fn timewarp_book(demo: &DemoData, book_title: &str, year: i64) -> Result<()> {
    if let Some(&book_id) = demo.books.get(book_title) {
        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!("║  TIME WARP: {} in {}", book_title.to_uppercase(), year);
        println!("╚═══════════════════════════════════════════════════════════╝");

        // Get current state
        let current = demo.db.get_node(book_id)?;
        let current_interp = current
            .properties
            .get("interpretation")
            .map(format_value)
            .unwrap_or_default();

        println!("\nBook: {}", book_title);
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

        // Use GallifreyDB's temporal API to get the node as it was in that year
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
    if let Some(&author_id) = demo.authors.get(author_name) {
        let author = demo.db.get_node(author_id)?;

        println!("\n╔═══════════════════════════════════════════════════════════╗");
        println!("║  INFLUENCE NETWORK: {}", author_name.to_uppercase());
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
        println!("\n{} ({})", author_name, years);

        // Find who influenced this author (incoming INFLUENCED_BY edges)
        println!("\n═══ INFLUENCED BY ═══");
        let mut found_influences = false;
        for edge_id in demo.db.get_incoming_edges(author_id) {
            if let Ok(edge) = demo.db.get_edge(edge_id)
                && label_str(edge.label) == "INFLUENCED_BY"
            {
                found_influences = true;
                let influencer = demo.db.get_node(edge.source)?;
                let influencer_name = influencer
                    .properties
                    .get("name")
                    .map(format_value)
                    .unwrap_or_default();
                let influence_type = edge
                    .properties
                    .get("type")
                    .map(format_value)
                    .unwrap_or_else(|| "general influence".to_string());
                println!("  ← {} ({})", influencer_name, influence_type);
            }
        }
        if !found_influences {
            println!("  (No recorded influences)");
        }

        // Find who this author influenced (outgoing INFLUENCED_BY edges)
        println!("\n═══ INFLUENCED ═══");
        let mut found_influenced = false;
        for edge_id in demo.db.get_outgoing_edges(author_id) {
            if let Ok(edge) = demo.db.get_edge(edge_id)
                && label_str(edge.label) == "INFLUENCED_BY"
            {
                found_influenced = true;
                let influenced = demo.db.get_node(edge.target)?;
                let influenced_name = influenced
                    .properties
                    .get("name")
                    .map(format_value)
                    .unwrap_or_default();
                let influence_type = edge
                    .properties
                    .get("type")
                    .map(format_value)
                    .unwrap_or_else(|| "general influence".to_string());
                println!("  → {} ({})", influenced_name, influence_type);
            }
        }
        if !found_influenced {
            println!("  (No recorded literary descendants)");
        }

        // Show their major works
        println!("\n═══ MAJOR WORKS ═══");
        let mut works = vec![];
        for edge_id in demo.db.get_outgoing_edges(author_id) {
            if let Ok(edge) = demo.db.get_edge(edge_id)
                && label_str(edge.label) == "WROTE"
            {
                let book = demo.db.get_node(edge.target)?;
                let title = book
                    .properties
                    .get("title")
                    .map(format_value)
                    .unwrap_or_default();
                let year = book
                    .properties
                    .get("published_year")
                    .map(format_value)
                    .unwrap_or_default();
                works.push((year.parse::<i64>().unwrap_or(0), title));
            }
        }
        works.sort_by_key(|(year, _)| *year);
        for (year, title) in works {
            println!("  • {} ({})", title, year);
        }
    } else {
        println!("\n❌ Author not found: {}", author_name);
        println!("\nTry: list authors");
    }

    Ok(())
}

fn show_semantic_drift(demo: &DemoData, character_name: &str) -> Result<()> {
    // Find character with fuzzy matching
    if let Some((name, &character_id)) = find_character_fuzzy(&demo.characters, character_name) {
        println!("\n=== Semantic Drift: {} ===\n", name);

        // Get the temporal vector index
        let temporal_index = demo.db.get_temporal_vector_index().ok_or_else(|| {
            gallifreydb::Error::other("Temporal vector index not enabled".to_string())
        })?;

        // Get the original (current) embedding
        let current_node = demo.db.get_node(character_id)?;
        let reference_embedding = if let Some(gallifreydb::PropertyValue::Vector(vec)) =
            current_node.properties.get("personality_embedding")
        {
            vec.clone()
        } else {
            println!("❌ No personality_embedding found for this character");
            return Ok(());
        };

        // Track drift from 1866 to 2024
        use gallifreydb::core::temporal::TimeRange;
        let time_range = TimeRange::new(
            year_to_timestamp(1866), // Start of our data
            now_timestamp()?,        // Current time
        )?;

        println!("Tracking semantic drift from original understanding to present:\n");

        // Get drift timeline (returns cosine similarity, not distance)
        match temporal_index.track_semantic_drift(character_id, &reference_embedding, time_range) {
            Ok(drift_timeline) => {
                let drift_vec: Vec<(gallifreydb::Timestamp, f32)> = drift_timeline;
                if drift_vec.is_empty() {
                    println!(
                        "  No temporal versions found (character may not have evolving embeddings)"
                    );
                } else {
                    println!("  Time Point              Cosine Distance  Interpretation");
                    println!("  ───────────────────────────────────────────────────────────");

                    for (timestamp, similarity) in drift_vec {
                        // Convert similarity to distance: distance = 1.0 - similarity
                        let distance = 1.0 - similarity;

                        // Convert timestamp to approximate year
                        let year = 1970 + (timestamp / (365 * 86400 * 1_000_000));

                        // Get personality at this point in time
                        let historical_node =
                            demo.db
                                .get_node_at_time(character_id, timestamp, now_timestamp()?)?;
                        let personality = historical_node
                            .properties
                            .get("personality")
                            .map(format_value)
                            .unwrap_or_else(|| "Unknown".to_string());

                        // Truncate personality for display
                        let personality_short = if personality.len() > 50 {
                            format!("{}...", &personality[..47])
                        } else {
                            personality
                        };

                        println!(
                            "  ~{:4}                  {:.4}           {}",
                            year, distance, personality_short
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
                println!("     (This may happen if temporal snapshots haven't been created yet)");
            }
        }
    } else {
        println!("\n❌ Character not found: {}", character_name);
        println!("\nTry: list characters");
    }

    Ok(())
}

fn show_personality_evolution(demo: &DemoData, character_name: &str) -> Result<()> {
    // Find character with fuzzy matching
    if let Some((name, &character_id)) = find_character_fuzzy(&demo.characters, character_name) {
        println!("\n=== Personality Evolution: {} ===\n", name);

        // Get all historical versions
        let current_time = now_timestamp()?;

        println!("How our understanding of this character evolved over time:\n");

        // We'll manually query specific years we know have versions
        let years = [1866, 1900, 1920, 1925, 1950, 1960, 1970, 2024];

        for &year in &years {
            let query_time = year_to_timestamp(year);

            match demo
                .db
                .get_node_at_time(character_id, query_time, current_time)
            {
                Ok(historical_node) => {
                    if let Some(personality) = historical_node.properties.get("personality") {
                        let personality_text = format_value(personality);
                        println!("┌─ {} {}", year, "─".repeat(60 - year.to_string().len()));
                        println!("│");
                        let wrapped = wrap_text(&personality_text, 70, "│ ");
                        println!("│ {}", wrapped);
                        println!("│");
                    }
                }
                Err(_) => {
                    // No version at this time, skip
                }
            }
        }

        println!("└{}\n", "─".repeat(65));
        println!("💡 This shows how literary criticism evolved from publication to present");
    } else {
        println!("\n❌ Character not found: {}", character_name);
        println!("\nTry: list characters");
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

SEMANTIC SEARCH (Vector Embeddings):
  similar <character>      - Find similar characters
  drift <character>        - Show semantic drift over time

TEMPORAL QUERIES (Time Travel):
  timewarp <book> <year>   - See how interpretation evolved
  evolution <character>    - Show personality evolution timeline

GRAPH QUERIES:
  influences <author>      - Show who influenced/was influenced

SYSTEM:
  stats                    - Database statistics
  help                     - Show this help
  quit / exit              - Exit demo

Examples:
  > show Fyodor Dostoevsky
  > similar Raskolnikov
  > drift Raskolnikov
  > evolution "Anna Karenina"
  > timewarp "Crime and Punishment" 1900
  > influences Dostoevsky
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
║   GallifreyDB Comprehensive Demo                          ║
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
        .map_err(|e| gallifreydb::Error::other(format!("Failed to initialize readline: {}", e)))?;
    rl.set_helper(Some(completer));

    println!("\n💡 Tip: Use TAB for auto-complete!");

    // Main REPL loop
    loop {
        let readline = rl.readline("\nrussian-lit> ");
        let input = match readline {
            Ok(line) => {
                rl.add_history_entry(&line).map_err(|e| {
                    gallifreydb::Error::other(format!("Failed to add history entry: {}", e))
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
            "similar" | "sim" => {
                if args.is_empty() {
                    println!("Usage: similar <character_name>");
                    println!("Example: similar Raskolnikov");
                    println!("\nTry: list characters");
                } else {
                    find_similar_characters(&demo, args, 5)?;
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
                    println!("\nTry: list authors");
                } else {
                    show_influences(&demo, args)?;
                }
            }
            "drift" => {
                if args.is_empty() {
                    println!("Usage: drift <character_name>");
                    println!("Example: drift Raskolnikov");
                    println!("\nShows semantic drift over time");
                    println!("Try: list characters");
                } else {
                    show_semantic_drift(&demo, args)?;
                }
            }
            "evolution" | "evo" => {
                if args.is_empty() {
                    println!("Usage: evolution <character_name>");
                    println!("Example: evolution \"Anna Karenina\"");
                    println!("\nShows personality evolution over time");
                    println!("Try: list characters");
                } else {
                    show_personality_evolution(&demo, args)?;
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
