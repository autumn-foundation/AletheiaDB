# Russian Writers Knowledge Graph - Design Document

## Overview

A comprehensive knowledge graph of Russian literary history (1799-1917) showcasing GallifreyDB's bi-temporal capabilities, vector embeddings, and hybrid graph+semantic queries.

## Why This Example?

**Rich Graph Structure:**
- 10+ major authors with complex influence networks
- 30+ major literary works spanning a century
- 50+ iconic characters with deep relationships
- Historical events, literary movements, themes

**Strong Temporal Dimension:**
- **Valid Time**: When works were published (1820s-1900s)
- **Transaction Time**: When literary criticism/interpretations evolved
- **Evolving Knowledge**: How understanding of works changed over 150+ years

**Perfect for Embeddings:**
- Character personality embeddings (semantic similarity)
- Thematic embeddings for books
- Writing style comparisons across authors
- Literary analysis across time periods

## Schema Design

### Node Labels

#### 1. **Author**
```rust
{
  label: "Author",
  properties: {
    name: String,              // "Fyodor Dostoevsky"
    birth_year: Int,           // 1821
    death_year: Int,           // 1881
    nationality: String,       // "Russian"
    biography: String,         // Full biography text
    writing_style: String,     // "Psychological realism, philosophical depth..."
    major_themes: String,      // "Redemption, suffering, faith..."
    wikipedia_url: String,
    style_embedding: Vector,   // Embedding of writing style
  }
}
```

#### 2. **Book**
```rust
{
  label: "Book",
  properties: {
    title: String,             // "Crime and Punishment"
    original_title: String,    // "Преступление и наказание"
    published_year: Int,       // 1866
    genre: String,             // "Philosophical novel"
    summary: String,           // Full plot summary
    themes: String,            // "Guilt, redemption, morality..."
    critical_reception: String, // Evolves over time!
    interpretation: String,     // Evolves over time!
    page_count: Int,
    wikipedia_url: String,
    theme_embedding: Vector,   // Embedding of themes/plot
  }
}
```

#### 3. **Character**
```rust
{
  label: "Character",
  properties: {
    name: String,              // "Rodion Raskolnikov"
    role: String,              // "Protagonist"
    description: String,       // Full character description
    personality: String,       // "Proud, isolated, conflicted..."
    arc: String,               // Character development arc
    significance: String,      // Literary significance
    personality_embedding: Vector, // Semantic personality
  }
}
```

#### 4. **Theme**
```rust
{
  label: "Theme",
  properties: {
    name: String,              // "Redemption"
    description: String,       // Detailed explanation
    examples: String,          // How it appears in literature
    theme_embedding: Vector,
  }
}
```

#### 5. **Movement**
```rust
{
  label: "Movement",
  properties: {
    name: String,              // "Realism"
    period: String,            // "1850s-1890s"
    characteristics: String,   // Key features
    key_figures: String,       // Major proponents
  }
}
```

#### 6. **HistoricalEvent**
```rust
{
  label: "HistoricalEvent",
  properties: {
    name: String,              // "Emancipation of Serfs"
    year: Int,                 // 1861
    description: String,
    significance: String,      // Impact on literature
  }
}
```

### Edge Labels

#### Author Relationships
- `WROTE` (Author → Book): `{year: Int, circumstances: String}`
- `INFLUENCED_BY` (Author → Author): `{type: String, period: String}`
- `CONTEMPORARY_OF` (Author → Author): `{overlapping_years: String}`
- `PART_OF_MOVEMENT` (Author → Movement): `{contribution: String}`
- `INFLUENCED_BY_EVENT` (Author → HistoricalEvent): `{impact: String}`

#### Book Relationships
- `APPEARS_IN` (Character → Book): `{role: String, significance: String}`
- `CONTAINS_THEME` (Book → Theme): `{prominence: String}`
- `INFLUENCED_BY_BOOK` (Book → Book): `{how: String}`
- `REFLECTS_EVENT` (Book → HistoricalEvent): `{connection: String}`

#### Character Relationships
- `SIMILAR_TO` (Character → Character): `{similarity_type: String, score: Float}` [semantic]
- `INTERACTS_WITH` (Character → Character): `{relationship: String}`
- `ARCHETYPE_OF` (Character → Theme): `{how: String}`

## Data Sources

### Primary Sources

1. **Wikipedia** (via API):
   - Author biographies
   - Book summaries and critical reception
   - Historical context
   - Character descriptions

2. **Project Gutenberg**:
   - Full text of public domain works
   - Extract character dialogues for embeddings

3. **SparkNotes / CliffsNotes Style Summaries**:
   - Character analyses
   - Theme descriptions

### Embedding Generation

**Using Ollama with all-minilm model (384 dimensions)**:

```python
# Style embedding (author)
style_text = f"{author.writing_style}. {author.major_themes}. {author.biography[:500]}"
style_embedding = ollama.embed(style_text)

# Theme embedding (book)
theme_text = f"{book.title}. {book.summary}. Themes: {book.themes}"
theme_embedding = ollama.embed(theme_text)

# Personality embedding (character)
personality_text = f"{char.name}: {char.personality}. {char.description}. {char.arc}"
personality_embedding = ollama.embed(personality_text)
```

## Temporal Evolution Examples

### 1. Evolving Critical Reception

```rust
// 1866: Initial publication
create_node(Book {
  title: "Crime and Punishment",
  critical_reception: "Mixed reviews; controversial themes",
  interpretation: "Psychological study of criminal mind"
})

// 1880s: Growing recognition (valid_time: 1885)
update_node(book_id, {
  critical_reception: "Masterwork of psychological realism",
  interpretation: "Exploration of moral philosophy and guilt"
}, valid_time: 1885)

// 1920s: Existentialist lens (valid_time: 1925)
update_node(book_id, {
  interpretation: "Precursor to existentialist philosophy; absurdist themes"
}, valid_time: 1925)

// 1950s: Freudian analysis (valid_time: 1955)
update_node(book_id, {
  interpretation: "Oedipal conflict; unconscious guilt; id vs superego"
}, valid_time: 1955)

// 2020s: Modern perspective (valid_time: 2024)
update_node(book_id, {
  interpretation: "Complex portrayal of mental illness, poverty, isolation"
}, valid_time: 2024)
```

### 2. Discovered Influences

```rust
// 1900: Initial understanding
create_edge(dostoevsky, gogol, "INFLUENCED_BY", {
  type: "narrative style",
  confidence: "known"
}, valid_time: 1900)

// 1950: Scholars discover deeper connection
create_edge(dostoevsky, pushkin, "INFLUENCED_BY", {
  type: "psychological depth",
  discovered: "literary scholarship 1950s"
}, valid_time: 1950)
```

## Example Queries

### 1. Semantic Character Similarity

```rust
// Find characters similar to Raskolnikov
let raskolnikov = db.find_node("Character", "name" == "Rodion Raskolnikov")?;
let similar = db.find_similar(raskolnikov, 5)?;
// Returns: Prince Myshkin (0.87), Ivan Karamazov (0.85), Pechorin (0.82)...
```

### 2. Temporal Query - Historical View

```rust
// What did critics think of Crime and Punishment in 1900 vs 2024?
let book = db.find_node("Book", "title" == "Crime and Punishment")?;

let view_1900 = db.as_of("1900-01-01").get_node(book)?;
println!("1900: {}", view_1900.properties.get("interpretation"));
// "Psychological study of criminal mind"

let view_2024 = db.as_of("2024-01-01").get_node(book)?;
println!("2024: {}", view_2024.properties.get("interpretation"));
// "Complex portrayal of mental illness, poverty, isolation"
```

### 3. Hybrid Graph + Vector Query

```rust
// Find books thematically similar to Crime and Punishment,
// written by authors influenced by Gogol, published before 1880

let cp = db.find_node("Book", "title" == "Crime and Punishment")?;
let gogol = db.find_node("Author", "name" == "Nikolai Gogol")?;

// Graph traversal
let influenced_authors = db
  .traverse(gogol, "INFLUENCED_BY")
  .filter(|n| n.label == "Author")?;

let their_books = influenced_authors
  .flat_map(|author| db.traverse(author, "WROTE")
    .filter(|book| book.properties.get("published_year")? < 1880))?;

// Vector ranking
let ranked = db.rank_by_similarity(
  their_books,
  cp.properties.get("theme_embedding")?,
  10
)?;
```

### 4. Semantic Drift Detection

```rust
// How did understanding of Anna Karenina evolve?
let book = db.find_node("Book", "title" == "Anna Karenina")?;
let drift = db.track_semantic_drift(
  book,
  "interpretation",
  TimeRange::new("1878-01-01", "2024-01-01"),
  granularity: 10  // 10 snapshots across time range
)?;

// Returns timeline of cosine distances showing interpretation shifts
for (timestamp, distance, interpretation) in drift {
  println!("{}: drift={:.3} - {}", timestamp, distance, interpretation);
}
```

### 5. Influence Network Analysis

```rust
// Who influenced Dostoevsky, and who did Dostoevsky influence?
let dostoevsky = db.find_node("Author", "name" == "Fyodor Dostoevsky")?;

// Influences (incoming)
let influences = db.traverse_incoming(dostoevsky, "INFLUENCED_BY")?;
println!("Influenced by: {:?}", influences);

// Impact (outgoing)
let impacted = db.traverse_outgoing(dostoevsky, "INFLUENCED_BY")?;
println!("Influenced: {:?}", impacted);

// Transitive influence (2-hop)
let indirect = db.traverse_2hop(dostoevsky, "INFLUENCED_BY")?;
println!("Indirect influence chain: {:?}", indirect);
```

## Implementation Plan

### Phase 1: Data Collection
1. Wikipedia API scraper for authors/books/characters
2. Data cleaning and structuring
3. Generate embeddings with Ollama

### Phase 2: Database Import
1. Create schema and nodes
2. Create relationships
3. Add temporal versions (simulating evolving interpretations)

### Phase 3: Interactive Demo
1. REPL with query examples
2. Visualization of results
3. Temporal query demonstrations

### Phase 4: Documentation
1. README with setup instructions
2. Query cookbook with examples
3. Architecture explanation

## Dataset Scope

### Authors (10)
1. Alexander Pushkin (1799-1837) - Romantic poet, father of Russian literature
2. Mikhail Lermontov (1814-1841) - Romantic poet and novelist
3. Nikolai Gogol (1809-1852) - Satirist, early realist
4. Ivan Turgenev (1818-1883) - Realist novelist
5. Fyodor Dostoevsky (1821-1881) - Psychological realist
6. Leo Tolstoy (1828-1910) - Epic novelist
7. Ivan Goncharov (1812-1891) - Realist novelist
8. Nikolai Chernyshevsky (1828-1889) - Radical novelist
9. Anton Chekhov (1860-1904) - Short story master, playwright
10. Maxim Gorky (1868-1936) - Social realist

### Books (30)
- Eugene Onegin (Pushkin, 1833)
- A Hero of Our Time (Lermontov, 1840)
- Dead Souls (Gogol, 1842)
- Fathers and Sons (Turgenev, 1862)
- Crime and Punishment (Dostoevsky, 1866)
- War and Peace (Tolstoy, 1869)
- The Idiot (Dostoevsky, 1869)
- Demons (Dostoevsky, 1872)
- Anna Karenina (Tolstoy, 1878)
- The Brothers Karamazov (Dostoevsky, 1880)
- The Cherry Orchard (Chekhov, 1904)
- Mother (Gorky, 1906)
- ... (20+ more)

### Characters (50+)
- Eugene Onegin, Tatyana Larina
- Pechorin, Bela, Maxim Maximych
- Chichikov, Sobakevich, Plyushkin
- Bazarov, Arkady Kirsanov
- Raskolnikov, Sonya Marmeladova, Porfiry Petrovich
- Pierre Bezukhov, Natasha Rostova, Prince Andrei
- Prince Myshkin, Nastasya Filippovna, Rogozhin
- Stavrogin, Pyotr Verkhovensky, Shatov
- Anna Karenina, Konstantin Levin, Alexei Vronsky
- Ivan Karamazov, Dmitri Karamazov, Alyosha Karamazov
- ... (40+ more)

### Themes (15)
- Redemption, Suffering, Faith, Love, Nihilism, Society, Fate, Freedom,
  Mortality, Identity, Alienation, Duty, Passion, Reason, Revolution

### Historical Events (10)
- Napoleonic Invasion of Russia (1812)
- Decembrist Revolt (1825)
- Crimean War (1853-1856)
- Emancipation of Serfs (1861)
- Assassination of Alexander II (1881)
- Russo-Japanese War (1904-1905)
- 1905 Revolution
- World War I (1914-1918)
- February Revolution (1917)
- October Revolution (1917)

### Literary Movements (5)
- Romanticism (1800-1850)
- Natural School / Early Realism (1840s-1850s)
- Realism (1850s-1880s)
- Symbolism (1880s-1910s)
- Socialist Realism (1900s-1930s)

## Expected Output

A complete, runnable example that demonstrates:

✅ Complex graph structure with rich relationships
✅ Vector embeddings for semantic search
✅ Temporal evolution of interpretations
✅ Hybrid graph+vector queries
✅ Real educational value (learn Russian literature!)
✅ Production-quality code (test-driven, well-documented)

## Success Metrics

- Query response times < 10ms for current state
- Semantic similarity results are accurate and meaningful
- Temporal queries show clear evolution of understanding
- Code is well-documented and easy to extend
- Dataset is comprehensive enough to be interesting
