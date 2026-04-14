# Russian Writers Knowledge Graph Example

A comprehensive demonstration of GallifreyDB's capabilities using Russian literary history (1799-1917).

## What This Example Demonstrates

✅ **Bi-Temporal Storage**: Track how literary interpretations evolved from publication to present day
✅ **Vector Embeddings**: Semantic search for similar characters, themes, and writing styles
✅ **Hybrid Queries**: Combine graph traversal with vector similarity ranking
✅ **Semantic Pathfinding**: Find meaningful connections guided by concepts (KILLER FEATURE! 🚀)
✅ **Rich Graph Structure**: Authors, books, characters, themes, historical events, movements
✅ **Real Educational Value**: Learn about Russian literature while exploring the database!

## Quick Start

### Prerequisites

1. **Rust** (latest stable)
2. **Python 3.8+** (for data fetching)
3. **Ollama** (for generating embeddings)

### Setup

1. **Install Ollama and download the model:**
```bash
# Install Ollama from https://ollama.ai
ollama pull all-minilm
ollama serve
```

2. **Install Python dependencies:**
```bash
cd examples/russian_writers
pip install requests ollama
```

3. **Fetch the data:**
```bash
python fetch_data.py
```

This will:
- Fetch data from Wikipedia for 9 authors, 15+ books, 20+ characters
- Generate embeddings using Ollama (384-dimensional vectors)
- Save structured JSON data to `data/` directory

4. **Run the example:**
```bash
cargo run --example russian_writers
```

Note: The example uses pre-generated embeddings from step 3, so no embedding feature flags are needed.

## Example Queries

Once the demo is running, you can try:

### Semantic Search
```
> similar raskolnikov
Finding characters similar to Rodion Raskolnikov...
1. Prince Myshkin (0.87) - Dostoevsky protagonist with moral struggles
2. Ivan Karamazov (0.85) - Intellectual torment and doubt
3. Pechorin (0.82) - Alienated, introspective hero
```

### Temporal Queries
```
> timewarp "Crime and Punishment" 1900
=== Crime and Punishment in 1900 ===
Critical reception: Masterwork of psychological realism
Interpretation: Exploration of moral philosophy and guilt

> timewarp "Crime and Punishment" 2024
=== Crime and Punishment in 2024 ===
Critical reception: One of the greatest novels ever written
Interpretation: Complex portrayal of mental illness, poverty, and isolation
```

### Hybrid Graph + Vector Queries
```
> influenced gogol similar "Dead Souls"
Finding books:
  1. By authors influenced by Gogol
  2. Similar to Dead Souls (thematically)

Results:
1. Crime and Punishment (0.78) - Fyodor Dostoevsky
2. Fathers and Sons (0.72) - Ivan Turgenev
3. Oblomov (0.68) - Ivan Goncharov
```

### Influence Networks
```
> influences dostoevsky
Influenced by:
  - Nikolai Gogol (psychological depth, grotesque imagery)
  - Alexander Pushkin (literary foundation)

Influenced:
  - Anton Chekhov (psychological realism)
  - Maxim Gorky (social themes)
```

### Semantic Pathfinding
```
> path Pushkin Gorky --like "Social Justice"
🎯 Finding path from Alexander Pushkin (Author) to Maxim Gorky (Author)
🧭 Guided by concept: Social Justice (Theme)

✅ Path found (3 hops):

  🟢 START: Alexander Pushkin [Author]
  ↓   INFLUENCED_BY   Nikolai Gogol [Author] (relevance: 0.756)
  ↓   INFLUENCED_BY   Ivan Turgenev [Author] (relevance: 0.823)
  🎯 END:   Maxim Gorky [Author] (relevance: 0.891)

💡 This path was chosen because each hop maximizes semantic similarity
   to the concept, creating a meaningful thematic connection!
```

## Dataset Overview

### Authors (9)
- Alexander Pushkin (1799-1837) - Father of Russian literature
- Mikhail Lermontov (1814-1841) - Romantic poet
- Nikolai Gogol (1809-1852) - Satirist, early realist
- Ivan Turgenev (1818-1883) - Realist novelist
- Fyodor Dostoevsky (1821-1881) - Psychological realist
- Leo Tolstoy (1828-1910) - Epic novelist
- Ivan Goncharov (1812-1891) - Realist
- Anton Chekhov (1860-1904) - Master of short stories
- Maxim Gorky (1868-1936) - Social realist

### Major Works (15)
- Eugene Onegin (Pushkin, 1833)
- A Hero of Our Time (Lermontov, 1840)
- Dead Souls (Gogol, 1842)
- Fathers and Sons (Turgenev, 1862)
- Crime and Punishment (Dostoevsky, 1866)
- War and Peace (Tolstoy, 1869)
- Anna Karenina (Tolstoy, 1878)
- The Brothers Karamazov (Dostoevsky, 1880)
- And more...

### Characters (20+)
- Eugene Onegin, Tatyana Larina (Eugene Onegin)
- Pechorin (A Hero of Our Time)
- Raskolnikov, Sonya (Crime and Punishment)
- Pierre, Natasha, Andrei (War and Peace)
- Anna Karenina, Levin (Anna Karenina)
- Ivan, Dmitri, Alyosha Karamazov (The Brothers Karamazov)
- And more...

## Architecture Highlights

### Data Model

```
Author --[WROTE]--> Book --[CONTAINS_THEME]--> Theme
  |                   |
  [INFLUENCED_BY]     [APPEARS_IN]
  |                   |
Author            Character --[SIMILAR_TO]--> Character
```

### Embeddings

1. **Author Style Embeddings** (384-dim)
   - Generated from: writing style + major themes + biography
   - Use case: Find authors with similar styles

2. **Book Theme Embeddings** (384-dim)
   - Generated from: title + summary + themes
   - Use case: Find thematically similar books

3. **Character Personality Embeddings** (384-dim)
   - Generated from: personality + description + character arc
   - Use case: Find similar character archetypes

### Temporal Evolution

The example simulates evolving literary criticism:

```rust
// 1866: Initial publication
create_book(Crime and Punishment, {
  interpretation: "Psychological study of criminal mind"
})

// 1885: Growing recognition
update_interpretation("Exploration of moral philosophy", valid_time: 1885)

// 1925: Existentialist reading
update_interpretation("Precursor to existentialism", valid_time: 1925)

// 1955: Freudian analysis
update_interpretation("Oedipal conflict; unconscious guilt", valid_time: 1955)

// 2024: Modern perspective
update_interpretation("Complex portrayal of mental illness", valid_time: 2024)
```

## Files

```
examples/russian_writers/
├── README.md              # This file
├── DESIGN.md              # Detailed design document
├── fetch_data.py          # Wikipedia + Ollama data fetcher
├── data/                  # Generated data (JSON)
│   ├── authors.json
│   ├── books.json
│   ├── characters.json
│   ├── themes.json
│   ├── movements.json
│   ├── events.json
│   └── relationships.json
└── russian_writers.rs     # Main Rust example (TBD)
```

## Learning Resources

Want to learn more about these authors and works?

- [Russian Literature on Wikipedia](https://en.wikipedia.org/wiki/Russian_literature)
- [Dostoevsky on Project Gutenberg](https://www.gutenberg.org/ebooks/author/314)
- [Tolstoy on Project Gutenberg](https://www.gutenberg.org/ebooks/author/128)

## Next Steps

After running this example:

1. Try the [LangChain integration](../../docs/guides/langchain-integration.md) to use GallifreyDB in RAG pipelines
2. Explore the [REST API](../../docs/API.md) for language-agnostic access
3. Check out the [MCP Server](../../docs/MCP.md) for Claude Desktop integration

## Contributing

Want to add more authors, books, or improve the embeddings? PRs welcome!

Ideas for expansion:
- Add 20th century Soviet literature (Bulgakov, Pasternak, Solzhenitsyn)
- Include poetry collections and analysis
- Add biographical events and their literary impact
- Expand character network with more relationships
