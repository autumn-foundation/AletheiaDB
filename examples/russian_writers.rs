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
//! Run with: cargo run --example russian_writers --features embedding-ollama

use gallifreydb::{
    GLOBAL_INTERNER, GallifreyDB, InternedString, NodeId, PropertyMapBuilder, Result, Timestamp,
    WriteOps,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
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
// Demo State
// ============================================================================

struct DemoData {
    db: GallifreyDB,
    nodes: HashMap<String, NodeId>,
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
            nodes: HashMap::new(),
            authors: HashMap::new(),
            books: HashMap::new(),
            characters: HashMap::new(),
            themes: HashMap::new(),
        }
    }

    fn get_node(&self, name: &str) -> Option<NodeId> {
        self.nodes.get(name).copied()
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get current timestamp in microseconds
#[allow(dead_code)]
fn now_timestamp() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as Timestamp
}

/// Create timestamp for a specific year (for temporal queries)
#[allow(dead_code)]
fn year_timestamp(year: i64) -> Timestamp {
    // Approximate: microseconds since epoch for Jan 1 of that year
    let days_since_1970 = (year - 1970) * 365;
    let seconds = days_since_1970 * 86400;
    (seconds * 1_000_000) as Timestamp
}

/// Helper to get label string from InternedString
fn label_str(label: InternedString) -> String {
    GLOBAL_INTERNER
        .resolve(label)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{:?}", label))
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
        demo.nodes.insert(author.name.clone(), node_id);
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
        demo.nodes.insert(book.title.clone(), node_id);
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
        demo.nodes.insert(character.name.clone(), node_id);
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
        demo.nodes.insert(theme.name.clone(), node_id);
        demo.themes.insert(theme.name, node_id);
    }

    // === CREATE MOVEMENTS ===
    println!("  Creating Literary Movements...");
    for movement in movements {
        let node_id = demo.db.create_node(
            "Movement",
            props! {
                "name" => movement.name.as_str(),
                "period" => movement.period.as_str(),
                "characteristics" => movement.characteristics.as_str(),
                "key_figures" => movement.key_figures.as_str(),
            },
        )?;
        demo.nodes.insert(movement.name, node_id);
    }

    // === CREATE HISTORICAL EVENTS ===
    println!("  Creating Historical Events...");
    for event in events {
        let node_id = demo.db.create_node(
            "HistoricalEvent",
            props! {
                "name" => event.name.as_str(),
                "year" => event.year,
                "description" => event.description.as_str(),
                "significance" => event.significance.as_str(),
            },
        )?;
        demo.nodes.insert(event.name, node_id);
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

TEMPORAL QUERIES (Time Travel):
  timewarp <book> <year>   - See how interpretation evolved

GRAPH QUERIES:
  influences <author>      - Show who influenced/was influenced

SYSTEM:
  stats                    - Database statistics
  help                     - Show this help
  quit / exit              - Exit demo

Examples:
  > show Fyodor Dostoevsky
  > similar Raskolnikov
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

    // Main REPL loop
    loop {
        print!("\nrussian-lit> ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_err() {
            break;
        }

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
            "similar" => {
                println!("TODO: Implement semantic similarity search");
                println!("This requires vector index support");
            }
            "timewarp" | "tw" => {
                println!("TODO: Implement temporal query");
                println!("This requires temporal query API");
            }
            "influences" | "inf" => {
                println!("TODO: Implement influence network traversal");
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
