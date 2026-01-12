#!/usr/bin/env python3
"""
Wikipedia Data Fetcher for Russian Writers Knowledge Graph

Fetches author, book, and character data from Wikipedia and generates
embeddings using Ollama for semantic search.

Requirements:
    pip install requests ollama

Usage:
    python fetch_data.py

Ensure Ollama is running with the all-minilm model:
    ollama pull all-minilm
    ollama serve
"""

import json
import re
import time
from typing import Dict, List, Optional, Any
from dataclasses import dataclass, asdict
from pathlib import Path

try:
    import requests
    import ollama
except ImportError:
    print("Error: Required packages not installed")
    print("Please run: pip install requests ollama")
    exit(1)


@dataclass
class Author:
    """Russian author with biography and style info"""
    name: str
    birth_year: int
    death_year: int
    nationality: str
    biography: str
    writing_style: str
    major_themes: str
    wikipedia_url: str
    style_embedding: Optional[List[float]] = None


@dataclass
class Book:
    """Literary work with themes and critical reception"""
    title: str
    original_title: str
    author: str
    published_year: int
    genre: str
    summary: str
    themes: str
    critical_reception: str
    interpretation: str
    wikipedia_url: str
    theme_embedding: Optional[List[float]] = None


@dataclass
class Character:
    """Literary character with personality traits"""
    name: str
    book: str
    author: str
    role: str
    description: str
    personality: str
    arc: str
    significance: str
    personality_embedding: Optional[List[float]] = None


@dataclass
class Theme:
    """Literary theme"""
    name: str
    description: str
    examples: str
    theme_embedding: Optional[List[float]] = None


@dataclass
class Movement:
    """Literary movement"""
    name: str
    period: str
    characteristics: str
    key_figures: str


@dataclass
class HistoricalEvent:
    """Historical event influencing literature"""
    name: str
    year: int
    description: str
    significance: str


class WikipediaFetcher:
    """Fetches data from Wikipedia API"""

    BASE_URL = "https://en.wikipedia.org/w/api.php"

    def __init__(self):
        self.session = requests.Session()
        self.session.headers.update({
            'User-Agent': 'GallifreyDB/1.0 (Educational Example)'
        })

    def get_page_content(self, title: str) -> Optional[str]:
        """Fetch the full text content of a Wikipedia page"""
        params = {
            'action': 'query',
            'format': 'json',
            'titles': title,
            'prop': 'extracts',
            'exintro': True,  # Just the introduction
            'explaintext': True,
        }

        try:
            response = self.session.get(self.BASE_URL, params=params)
            response.raise_for_status()
            data = response.json()

            pages = data['query']['pages']
            page = next(iter(pages.values()))

            if 'extract' in page:
                return page['extract']
            return None
        except Exception as e:
            print(f"  Warning: Failed to fetch {title}: {e}")
            return None

    def get_page_url(self, title: str) -> str:
        """Get the URL for a Wikipedia page"""
        return f"https://en.wikipedia.org/wiki/{title.replace(' ', '_')}"


class EmbeddingGenerator:
    """Generates embeddings using Ollama"""

    def __init__(self, model: str = "all-minilm"):
        self.model = model
        self._test_connection()

    def _test_connection(self):
        """Test that Ollama is running and model is available"""
        try:
            ollama.list()
            print(f"✓ Connected to Ollama")
        except Exception as e:
            print(f"Error: Cannot connect to Ollama")
            print(f"Please ensure Ollama is running: ollama serve")
            print(f"And the model is pulled: ollama pull {self.model}")
            raise e

    def embed(self, text: str) -> List[float]:
        """Generate embedding for text"""
        try:
            # Truncate very long texts to avoid timeouts
            if len(text) > 8000:
                text = text[:8000]

            response = ollama.embeddings(
                model=self.model,
                prompt=text
            )
            return response['embedding']
        except Exception as e:
            print(f"  Warning: Failed to generate embedding: {e}")
            # Return zero vector as fallback
            return [0.0] * 384  # all-minilm has 384 dimensions


class DataCollector:
    """Collects and structures data from Wikipedia"""

    def __init__(self):
        self.wiki = WikipediaFetcher()
        self.embeddings = EmbeddingGenerator()
        self.data_dir = Path(__file__).parent / "data"
        self.data_dir.mkdir(exist_ok=True)

    def collect_all(self):
        """Collect all data"""
        print("=" * 60)
        print("RUSSIAN WRITERS KNOWLEDGE GRAPH - DATA COLLECTION")
        print("=" * 60)
        print()

        authors = self.collect_authors()
        self.save_json("authors.json", [asdict(a) for a in authors])

        books = self.collect_books()
        self.save_json("books.json", [asdict(b) for b in books])

        characters = self.collect_characters()
        self.save_json("characters.json", [asdict(c) for c in characters])

        themes = self.collect_themes()
        self.save_json("themes.json", [asdict(t) for t in themes])

        movements = self.collect_movements()
        self.save_json("movements.json", [asdict(m) for m in movements])

        events = self.collect_events()
        self.save_json("events.json", [asdict(e) for e in events])

        # Generate relationship data
        relationships = self.generate_relationships(authors, books, characters)
        self.save_json("relationships.json", relationships)

        print()
        print("=" * 60)
        print("DATA COLLECTION COMPLETE!")
        print("=" * 60)
        print(f"\nData saved to: {self.data_dir}")
        print(f"  - {len(authors)} authors")
        print(f"  - {len(books)} books")
        print(f"  - {len(characters)} characters")
        print(f"  - {len(themes)} themes")
        print(f"  - {len(movements)} movements")
        print(f"  - {len(events)} historical events")
        print(f"  - {sum(len(v) for v in relationships.values())} relationships")

    def collect_authors(self) -> List[Author]:
        """Collect author data"""
        print("\n[1/6] Collecting Authors...")
        print("-" * 60)

        # Define authors with key info
        author_specs = [
            ("Alexander Pushkin", 1799, 1837, "Romantic poetry, father of Russian literature",
             "Romanticism, nationalism, love, fate, freedom"),
            ("Mikhail Lermontov", 1814, 1841, "Romantic poet and novelist, psychological depth",
             "Alienation, individualism, nature, fate, Byronic heroes"),
            ("Nikolai Gogol", 1809, 1852, "Satirical realism, grotesque imagery, social criticism",
             "Bureaucracy, absurdity, corruption, human folly"),
            ("Ivan Turgenev", 1818, 1883, "Lyrical realism, character-driven narratives",
             "Social change, love, nature, idealism vs nihilism"),
            ("Fyodor Dostoevsky", 1821, 1881, "Psychological realism, philosophical depth, existential themes",
             "Redemption, suffering, faith, morality, psychology, free will"),
            ("Leo Tolstoy", 1828, 1910, "Epic realism, moral philosophy, historical sweep",
             "History, morality, spirituality, family, society, death"),
            ("Ivan Goncharov", 1812, 1891, "Realist prose, social types, detailed character studies",
             "Sloth, Russian character, modernization, tradition"),
            ("Anton Chekhov", 1860, 1904, "Psychological realism, subtext, everyday tragedy",
             "Ennui, unrequited love, social change, human nature"),
            ("Maxim Gorky", 1868, 1936, "Social realism, proletarian themes, revolutionary spirit",
             "Class struggle, poverty, revolution, dignity of labor"),
        ]

        authors = []
        for name, birth, death, style, themes in author_specs:
            print(f"  Fetching: {name}...")

            # Fetch biography from Wikipedia
            content = self.wiki.get_page_content(name) or \
                     f"{name} was a Russian writer who lived from {birth} to {death}."

            # Create author
            author = Author(
                name=name,
                birth_year=birth,
                death_year=death,
                nationality="Russian",
                biography=content[:1000],  # Truncate for embedding
                writing_style=style,
                major_themes=themes,
                wikipedia_url=self.wiki.get_page_url(name),
            )

            # Generate style embedding
            style_text = f"{author.writing_style}. {author.major_themes}. {author.biography}"
            print(f"    Generating embedding...")
            author.style_embedding = self.embeddings.embed(style_text)

            authors.append(author)
            time.sleep(0.5)  # Be nice to Wikipedia

        print(f"✓ Collected {len(authors)} authors")
        return authors

    def collect_books(self) -> List[Book]:
        """Collect book data"""
        print("\n[2/6] Collecting Books...")
        print("-" * 60)

        # Define major works
        book_specs = [
            # (title, original, author, year, genre, themes)
            ("Eugene Onegin", "Евгений Онегин", "Alexander Pushkin", 1833, "Novel in verse",
             "Love, fate, societal expectations, authenticity, missed opportunities"),
            ("A Hero of Our Time", "Герой нашего времени", "Mikhail Lermontov", 1840, "Psychological novel",
             "Alienation, Byronic hero, fatalism, boredom, nihilism"),
            ("Dead Souls", "Мёртвые души", "Nikolai Gogol", 1842, "Satirical novel",
             "Corruption, bureaucracy, Russian character, greed, absurdity"),
            ("The Overcoat", "Шинель", "Nikolai Gogol", 1842, "Short story",
             "Social injustice, alienation, materialism, human dignity"),
            ("Fathers and Sons", "Отцы и дети", "Ivan Turgenev", 1862, "Realist novel",
             "Generational conflict, nihilism, love, tradition vs progress"),
            ("Crime and Punishment", "Преступление и наказание", "Fyodor Dostoevsky", 1866, "Psychological novel",
             "Guilt, redemption, morality, suffering, nihilism, alienation"),
            ("The Idiot", "Идиот", "Fyodor Dostoevsky", 1869, "Philosophical novel",
             "Innocence, passion, society, goodness, tragedy"),
            ("War and Peace", "Война и мир", "Leo Tolstoy", 1869, "Epic novel",
             "History, fate, free will, family, love, death, meaning of life"),
            ("Demons", "Бесы", "Fyodor Dostoevsky", 1872, "Political novel",
             "Nihilism, radicalism, faith, possession, chaos"),
            ("Anna Karenina", "Анна Каренина", "Leo Tolstoy", 1878, "Realist novel",
             "Love, passion, society, family, adultery, faith, suicide"),
            ("The Brothers Karamazov", "Братья Карамазовы", "Fyodor Dostoevsky", 1880, "Philosophical novel",
             "Faith, doubt, morality, free will, suffering, redemption, parricide"),
            ("Oblomov", "Обломов", "Ivan Goncharov", 1859, "Realist novel",
             "Sloth, Russian character, dreaminess, inaction, modernization"),
            ("The Cherry Orchard", "Вишнёвый сад", "Anton Chekhov", 1904, "Play",
             "Social change, nostalgia, loss, class, impermanence"),
            ("Uncle Vanya", "Дядя Ваня", "Anton Chekhov", 1898, "Play",
             "Ennui, unrequited love, wasted life, meaning, regret"),
            ("Mother", "Мать", "Maxim Gorky", 1906, "Political novel",
             "Revolution, class struggle, awakening, sacrifice, solidarity"),
        ]

        books = []
        for title, original, author, year, genre, themes in book_specs:
            print(f"  Fetching: {title}...")

            # Fetch summary from Wikipedia
            content = self.wiki.get_page_content(title) or \
                     f"{title} is a {genre} by {author}, published in {year}."

            # Create book with evolving interpretation
            book = Book(
                title=title,
                original_title=original,
                author=author,
                published_year=year,
                genre=genre,
                summary=content[:1500],
                themes=themes,
                critical_reception=f"Major work of Russian literature, published {year}",
                interpretation=f"A profound exploration of {themes}",
                wikipedia_url=self.wiki.get_page_url(title),
            )

            # Generate theme embedding
            theme_text = f"{book.title}. {book.summary}. Themes: {book.themes}"
            print(f"    Generating embedding...")
            book.theme_embedding = self.embeddings.embed(theme_text)

            books.append(book)
            time.sleep(0.5)

        print(f"✓ Collected {len(books)} books")
        return books

    def collect_characters(self) -> List[Character]:
        """Collect character data"""
        print("\n[3/6] Collecting Characters...")
        print("-" * 60)

        # Define major characters
        char_specs = [
            # (name, book, author, role, personality, arc, significance)
            ("Eugene Onegin", "Eugene Onegin", "Alexander Pushkin", "Protagonist",
             "Cynical, bored, sophisticated, emotionally detached, world-weary",
             "Rejects Tatyana's love, later regrets it when she rejects him",
             "Archetype of the 'superfluous man' in Russian literature"),

            ("Tatyana Larina", "Eugene Onegin", "Alexander Pushkin", "Female lead",
             "Romantic, sincere, passionate, principled, strong-willed",
             "From naive romantic to dignified married woman who chooses duty over passion",
             "Ideal of Russian womanhood; moral strength"),

            ("Pechorin", "A Hero of Our Time", "Mikhail Lermontov", "Protagonist",
             "Cynical, manipulative, intelligent, bored, self-destructive, Byronic",
             "Moves through encounters leaving destruction, unable to find meaning",
             "Embodies the 'superfluous man'; psychological complexity"),

            ("Chichikov", "Dead Souls", "Nikolai Gogol", "Protagonist",
             "Cunning, opportunistic, adaptable, superficially charming, greedy",
             "Travels buying 'dead souls' for fraudulent scheme",
             "Satirizes Russian bureaucracy and corruption"),

            ("Akaky Akakievich", "The Overcoat", "Nikolai Gogol", "Protagonist",
             "Meek, poor, dedicated clerk, simple, socially invisible",
             "Obsesses over new coat, loses it, dies of grief, returns as ghost",
             "Symbol of the little man crushed by society"),

            ("Bazarov", "Fathers and Sons", "Ivan Turgenev", "Protagonist",
             "Nihilistic, rational, arrogant, principled, rejects tradition and emotion",
             "Challenges old values, falls in love (contradicting his beliefs), dies of typhus",
             "Embodiment of nihilism and generational conflict"),

            ("Rodion Raskolnikov", "Crime and Punishment", "Fyodor Dostoevsky", "Protagonist",
             "Proud, isolated, intellectual, conflicted, tormented, philosophical",
             "Murders pawnbroker to test theory, consumed by guilt, finds redemption through love",
             "Exploration of moral philosophy, guilt, and redemption"),

            ("Sonya Marmeladova", "Crime and Punishment", "Fyodor Dostoevsky", "Supporting",
             "Compassionate, self-sacrificing, faithful, humble, prostitute forced by poverty",
             "Guides Raskolnikov toward redemption through faith and love",
             "Symbol of Christian redemption and unconditional love"),

            ("Prince Myshkin", "The Idiot", "Fyodor Dostoevsky", "Protagonist",
             "Innocent, compassionate, epileptic, Christ-like, naive, pure-hearted",
             "Tries to bring goodness to corrupt society, fails, returns to madness",
             "Exploration of innocence and goodness in a fallen world"),

            ("Nastasya Filippovna", "The Idiot", "Fyodor Dostoevsky", "Female lead",
             "Beautiful, traumatized, proud, self-destructive, torn between love and vengeance",
             "Cannot accept love, chooses self-destruction, murdered",
             "Tragedy of damaged innocence"),

            ("Pierre Bezukhov", "War and Peace", "Leo Tolstoy", "Protagonist",
             "Idealistic, philosophical, searching for meaning, clumsy, sincere",
             "From aimless wealthy heir to man finding purpose through suffering and love",
             "Tolstoy's voice on meaning of life"),

            ("Natasha Rostova", "War and Peace", "Leo Tolstoy", "Female lead",
             "Vivacious, emotional, impulsive, loving, vital, transforms through experience",
             "From spirited girl to mature woman and mother",
             "Embodiment of life force and Russian spirit"),

            ("Prince Andrei Bolkonsky", "War and Peace", "Leo Tolstoy", "Major character",
             "Proud, ambitious, disillusioned, intelligent, searching for glory",
             "Seeks glory in war, finds it hollow, finds peace before death",
             "Exploration of pride, death, and meaning"),

            ("Anna Karenina", "Anna Karenina", "Leo Tolstoy", "Protagonist",
             "Passionate, impulsive, loving, trapped by society, increasingly desperate",
             "Leaves husband for lover, rejected by society, descends into jealousy and despair, suicide",
             "Tragedy of passion vs societal duty"),

            ("Konstantin Levin", "Anna Karenina", "Leo Tolstoy", "Parallel protagonist",
             "Philosophical, sincere, searches for meaning, loves nature and work",
             "Finds meaning in family, work, and faith",
             "Tolstoy's voice on authentic life"),

            ("Ivan Karamazov", "The Brothers Karamazov", "Fyodor Dostoevsky", "Major character",
             "Intellectual, atheistic, tormented by doubt, cold, rationalist",
             "Questions God's justice, suffers mental breakdown after father's murder",
             "Intellectual rebellion against God"),

            ("Dmitri Karamazov", "The Brothers Karamazov", "Fyodor Dostoevsky", "Major character",
             "Passionate, impulsive, sensual, generous, volatile",
             "Accused of patricide, accepts suffering as redemption",
             "Embodiment of passion and Karamazov sensuality"),

            ("Alyosha Karamazov", "The Brothers Karamazov", "Fyodor Dostoevsky", "Youngest brother",
             "Pure, loving, faithful, compassionate, novice monk",
             "Maintains faith despite suffering, spreads love",
             "Dostoevsky's ideal of Christian love"),

            ("Ilya Oblomov", "Oblomov", "Ivan Goncharov", "Protagonist",
             "Lazy, dreamy, passive, kind-hearted, paralyzed by inaction",
             "Cannot leave his couch, loses love, dies unfulfilled",
             "Symbol of Russian inertia and wasted potential"),
        ]

        characters = []
        for name, book, author, role, personality, arc, significance in char_specs:
            print(f"  Creating: {name}...")

            # For characters, we don't need Wikipedia; we have rich descriptions
            description = f"{name} appears in {book} by {author} as a {role}. {significance}"

            character = Character(
                name=name,
                book=book,
                author=author,
                role=role,
                description=description,
                personality=personality,
                arc=arc,
                significance=significance,
            )

            # Generate personality embedding
            personality_text = f"{character.name}: {character.personality}. {character.description}. {character.arc}"
            print(f"    Generating embedding...")
            character.personality_embedding = self.embeddings.embed(personality_text)

            characters.append(character)
            time.sleep(0.2)

        print(f"✓ Collected {len(characters)} characters")
        return characters

    def collect_themes(self) -> List[Theme]:
        """Collect literary themes"""
        print("\n[4/6] Collecting Themes...")
        print("-" * 60)

        theme_specs = [
            ("Redemption", "The possibility of moral and spiritual recovery after sin or suffering",
             "Raskolnikov's redemption through suffering and love (Crime and Punishment)"),
            ("Suffering", "Physical, emotional, or spiritual pain as path to meaning or growth",
             "Russian literature often presents suffering as redemptive (Dostoevsky, Tolstoy)"),
            ("Faith", "Religious belief and its role in providing meaning and moral guidance",
             "Alyosha's faith (Brothers Karamazov), Levin's spiritual quest (Anna Karenina)"),
            ("Love", "Romantic, familial, or spiritual love as transformative force",
             "Tatyana's love (Eugene Onegin), Anna's passion (Anna Karenina)"),
            ("Nihilism", "Rejection of traditional values and authority",
             "Bazarov (Fathers and Sons), Raskolnikov's theory (Crime and Punishment)"),
            ("Society", "Social structures, class, and their constraints on individuals",
             "Anna's ostracism (Anna Karenina), Pechorin's alienation (A Hero of Our Time)"),
            ("Fate", "Question of free will vs predestination",
             "Pierre's search for purpose (War and Peace), Pechorin's fatalism"),
            ("Freedom", "Personal liberty vs social responsibility",
             "Eugene Onegin's rejection of society, Raskolnikov's theory of extraordinary men"),
            ("Mortality", "Death and the meaning of existence",
             "Prince Andrei's death epiphany (War and Peace), Anna's suicide (Anna Karenina)"),
            ("Alienation", "Isolation from society and self",
             "Superfluous man archetype (Pechorin, Onegin), Akaky (The Overcoat)"),
            ("Duty", "Moral or social obligation",
             "Tatyana chooses duty over passion (Eugene Onegin)"),
            ("Passion", "Intense emotion that overrides reason",
             "Anna's passionate love (Anna Karenina), Dmitri's sensuality (Brothers Karamazov)"),
        ]

        themes = []
        for name, description, examples in theme_specs:
            print(f"  Creating: {name}...")

            theme = Theme(
                name=name,
                description=description,
                examples=examples,
            )

            # Generate theme embedding
            theme_text = f"{theme.name}: {theme.description}. Examples: {theme.examples}"
            theme.theme_embedding = self.embeddings.embed(theme_text)

            themes.append(theme)

        print(f"✓ Collected {len(themes)} themes")
        return themes

    def collect_movements(self) -> List[Movement]:
        """Collect literary movements"""
        print("\n[5/6] Collecting Literary Movements...")
        print("-" * 60)

        movements = [
            Movement(
                name="Romanticism",
                period="1800-1850",
                characteristics="Emotion, individualism, nature, idealized past, heroic individuals, exotic settings",
                key_figures="Pushkin, Lermontov, early Gogol"
            ),
            Movement(
                name="Realism",
                period="1850-1890",
                characteristics="Detailed depiction of reality, social issues, psychological depth, rejection of romanticism",
                key_figures="Turgenev, Dostoevsky, Tolstoy, Goncharov, Chekhov"
            ),
            Movement(
                name="Psychological Realism",
                period="1860-1890",
                characteristics="Deep exploration of consciousness, moral psychology, existential questions",
                key_figures="Dostoevsky, Tolstoy, Turgenev"
            ),
            Movement(
                name="Social Realism",
                period="1890-1930",
                characteristics="Focus on social conditions, working class, revolutionary themes",
                key_figures="Gorky, Chekhov"
            ),
        ]

        print(f"✓ Collected {len(movements)} movements")
        return movements

    def collect_events(self) -> List[HistoricalEvent]:
        """Collect historical events"""
        print("\n[6/6] Collecting Historical Events...")
        print("-" * 60)

        events = [
            HistoricalEvent(
                name="Napoleonic Invasion of Russia",
                year=1812,
                description="Napoleon's invasion and retreat from Russia",
                significance="Central event in War and Peace; shaped Russian national identity"
            ),
            HistoricalEvent(
                name="Decembrist Revolt",
                year=1825,
                description="Failed uprising by liberal nobles against tsarist autocracy",
                significance="Influenced Pushkin and generation of writers; symbolized idealism vs autocracy"
            ),
            HistoricalEvent(
                name="Emancipation of Serfs",
                year=1861,
                description="Alexander II freed Russian serfs",
                significance="Major social upheaval; backdrop for Turgenev, Dostoevsky, Tolstoy"
            ),
            HistoricalEvent(
                name="Assassination of Alexander II",
                year=1881,
                description="Tsar killed by revolutionary terrorists",
                significance="Led to repression; backdrop for nihilism in literature"
            ),
            HistoricalEvent(
                name="1905 Revolution",
                year=1905,
                description="Failed revolution against tsarist autocracy",
                significance="Influenced Gorky's revolutionary themes; foreshadowed 1917"
            ),
        ]

        print(f"✓ Collected {len(events)} historical events")
        return events

    def generate_relationships(self, authors: List[Author],
                              books: List[Book],
                              characters: List[Character]) -> Dict[str, List[Dict[str, Any]]]:
        """Generate relationship data between entities"""
        print("\nGenerating Relationships...")
        print("-" * 60)

        relationships = {
            "wrote": [],
            "influenced_by": [],
            "appears_in": [],
            "contains_theme": [],
            "similar_to": [],
        }

        # WROTE relationships
        for book in books:
            relationships["wrote"].append({
                "author": book.author,
                "book": book.title,
                "year": book.published_year,
            })

        # INFLUENCED_BY (author influences)
        influences = [
            ("Mikhail Lermontov", "Alexander Pushkin", "romantic style"),
            ("Nikolai Gogol", "Alexander Pushkin", "narrative innovation"),
            ("Ivan Turgenev", "Nikolai Gogol", "realist observation"),
            ("Fyodor Dostoevsky", "Nikolai Gogol", "psychological depth"),
            ("Leo Tolstoy", "Alexander Pushkin", "literary foundation"),
            ("Anton Chekhov", "Ivan Turgenev", "character-driven narratives"),
            ("Maxim Gorky", "Anton Chekhov", "social realism"),
        ]

        for source, target, influence_type in influences:
            relationships["influenced_by"].append({
                "source": source,
                "target": target,
                "type": influence_type,
            })

        # APPEARS_IN relationships
        for character in characters:
            relationships["appears_in"].append({
                "character": character.name,
                "book": character.book,
                "role": character.role,
            })

        # CONTAINS_THEME relationships (books to themes)
        book_themes = [
            ("Eugene Onegin", ["Love", "Fate", "Alienation", "Duty"]),
            ("A Hero of Our Time", ["Alienation", "Fate", "Nihilism"]),
            ("Dead Souls", ["Society", "Alienation"]),
            ("Crime and Punishment", ["Redemption", "Suffering", "Nihilism", "Alienation", "Mortality"]),
            ("War and Peace", ["Fate", "Freedom", "Love", "Mortality", "Society"]),
            ("Anna Karenina", ["Love", "Passion", "Society", "Duty", "Mortality"]),
            ("The Brothers Karamazov", ["Faith", "Suffering", "Freedom", "Mortality", "Redemption"]),
            ("The Idiot", ["Faith", "Love", "Society", "Suffering"]),
        ]

        for book_title, themes in book_themes:
            for theme in themes:
                relationships["contains_theme"].append({
                    "book": book_title,
                    "theme": theme,
                })

        # SIMILAR_TO relationships (characters with similar personalities)
        # Based on our embeddings and literary analysis
        similar_characters = [
            ("Eugene Onegin", "Pechorin", "superfluous man archetype"),
            ("Rodion Raskolnikov", "Prince Myshkin", "Dostoevsky protagonists with moral struggles"),
            ("Rodion Raskolnikov", "Ivan Karamazov", "intellectual torment"),
            ("Pierre Bezukhov", "Konstantin Levin", "Tolstoy heroes seeking meaning"),
            ("Anna Karenina", "Nastasya Filippovna", "passionate women destroyed by society"),
        ]

        for char1, char2, similarity_type in similar_characters:
            relationships["similar_to"].append({
                "character1": char1,
                "character2": char2,
                "type": similarity_type,
            })

        total = sum(len(v) for v in relationships.values())
        print(f"✓ Generated {total} relationships")

        return relationships

    def save_json(self, filename: str, data: Any):
        """Save data to JSON file"""
        filepath = self.data_dir / filename
        with open(filepath, 'w', encoding='utf-8') as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
        print(f"  Saved: {filename}")


def main():
    """Main entry point"""
    try:
        collector = DataCollector()
        collector.collect_all()
    except KeyboardInterrupt:
        print("\n\nInterrupted by user")
    except Exception as e:
        print(f"\n\nError: {e}")
        import traceback
        traceback.print_exc()


if __name__ == "__main__":
    main()
