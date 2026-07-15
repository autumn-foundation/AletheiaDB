//! Built-in synthetic, SNB-shaped graph generator.
//!
//! This produces a deterministic (seeded) social-network graph in the shape of
//! the LDBC SNB schema subset the suite exercises: `Person`, `Forum`, `Post`,
//! `Comment`, `Tag`, and the relationships between them. It ships so the suite
//! runs with **zero external dependencies**.
//!
//! ## Honesty
//!
//! This is **NOT** the official LDBC Datagen (Spark) output. The official
//! generator produces power-law degree distributions, realistic attribute
//! correlations, and audited scale factors. This built-in generator produces a
//! simpler, uniform-random-ish graph at scale points we label
//! `"sf0.1-equivalent"` and `"smoke"` — deliberately *shaped like* SNB but not
//! claiming its statistical fidelity. To run against the real generator,
//! feed its CSV output through the (documented) CSV loader path instead — see
//! `README.md` and `docs/METHODOLOGY.md`.
//!
//! The generator output is a pure data description; [`crate::loader`] turns it
//! into an AletheiaDB instance.

use crate::prng::SplitMix64;
use serde::Serialize;

/// The scale point to generate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// Tiny size for smoke tests and CI (a few dozen persons).
    Smoke,
    /// "SF0.1-equivalent": a small but non-trivial size. NOT the official
    /// LDBC SF0.1 person count — see module docs.
    Sf01,
}

impl Scale {
    /// Parse a scale from its CLI string form.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "smoke" => Some(Self::Smoke),
            "sf0.1" | "sf01" | "sf0_1" => Some(Self::Sf01),
            _ => None,
        }
    }

    /// The canonical string label used in reports.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Sf01 => "sf0.1",
        }
    }

    /// The concrete generation parameters for this scale.
    #[must_use]
    pub fn params(self) -> GenParams {
        match self {
            Self::Smoke => GenParams {
                persons: 60,
                avg_knows_degree: 6,
                forums: 6,
                posts_per_forum: 8,
                comments_per_post: 2,
                tags: 12,
                // Vector-extension embeddings are attached to Posts. Smoke keeps
                // this tiny; the real vector scale run is reported honestly.
                embedding_dim: 32,
                // Number of extra update-stream revisions applied to a subset of
                // persons (builds bi-temporal history for the temporal extension).
                update_revisions: 3,
            },
            Self::Sf01 => GenParams {
                persons: 1_500,
                avg_knows_degree: 14,
                forums: 60,
                posts_per_forum: 40,
                comments_per_post: 3,
                tags: 120,
                embedding_dim: 64,
                update_revisions: 4,
            },
        }
    }
}

/// Concrete generation parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenParams {
    /// Number of `Person` nodes.
    pub persons: usize,
    /// Target average out-degree of the `KNOWS` relationship.
    pub avg_knows_degree: usize,
    /// Number of `Forum` nodes.
    pub forums: usize,
    /// `Post` nodes created per forum.
    pub posts_per_forum: usize,
    /// `Comment` nodes created per post.
    pub comments_per_post: usize,
    /// Number of `Tag` nodes.
    pub tags: usize,
    /// Embedding dimensionality for the vector extension.
    pub embedding_dim: usize,
    /// Update-stream revisions applied per selected person.
    pub update_revisions: usize,
}

/// A generated person.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenPerson {
    /// Zero-based index (stable identity within the generated graph).
    pub idx: usize,
    /// Display name (`Person <idx>`).
    pub first_name: String,
    /// Deterministic "city" bucket.
    pub city: String,
    /// Deterministic pseudo-birth-year.
    pub birth_year: i64,
}

/// A generated forum.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenForum {
    /// Zero-based index.
    pub idx: usize,
    /// Forum title.
    pub title: String,
    /// Moderator person index.
    pub moderator: usize,
}

/// A generated post.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenPost {
    /// Zero-based index.
    pub idx: usize,
    /// Owning forum index.
    pub forum: usize,
    /// Creator person index.
    pub creator: usize,
    /// Tag index attached to this post.
    pub tag: usize,
    /// Deterministic content length (a stand-in property).
    pub length: i64,
    /// Deterministic embedding vector for the vector extension.
    pub embedding: Vec<f32>,
}

/// A generated comment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenComment {
    /// Zero-based index.
    pub idx: usize,
    /// The post this comment replies to.
    pub reply_to_post: usize,
    /// Creator person index.
    pub creator: usize,
}

/// A single update-stream revision to a person's property (builds history).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GenPersonRevision {
    /// Target person index.
    pub person: usize,
    /// Revision number (1-based; revision N is applied after revision N-1).
    pub revision: usize,
    /// New city value at this revision.
    pub city: String,
}

/// The full generated graph (pure data; no DB dependency).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GeneratedGraph {
    /// The scale label.
    pub scale: String,
    /// The seed used.
    pub seed: u64,
    /// Generation parameters.
    #[serde(skip)]
    pub params: GenParams,
    /// Person nodes.
    pub persons: Vec<GenPerson>,
    /// Directed `KNOWS` edges as `(source_idx, target_idx)`.
    pub knows: Vec<(usize, usize)>,
    /// Forum nodes.
    pub forums: Vec<GenForum>,
    /// Post nodes.
    pub posts: Vec<GenPost>,
    /// Comment nodes.
    pub comments: Vec<GenComment>,
    /// Tag names (index-addressed).
    pub tags: Vec<String>,
    /// Update-stream person revisions, in application order.
    pub person_revisions: Vec<GenPersonRevision>,
}

const CITIES: &[&str] = &[
    "Springfield",
    "Rivertown",
    "Lakeside",
    "Hillcrest",
    "Fairview",
    "Brookfield",
    "Ashford",
    "Millbrook",
];

/// Deterministically generate a graph for the given scale and seed.
#[must_use]
pub fn generate(scale: Scale, seed: u64) -> GeneratedGraph {
    let params = scale.params();
    let mut rng = SplitMix64::new(seed);

    // --- Tags ---
    let tags: Vec<String> = (0..params.tags).map(|i| format!("Tag{i}")).collect();

    // --- Persons ---
    let persons: Vec<GenPerson> = (0..params.persons)
        .map(|idx| {
            let city = CITIES[rng.next_below(CITIES.len())].to_string();
            let birth_year = 1950 + rng.next_below(55) as i64;
            GenPerson {
                idx,
                first_name: format!("Person{idx}"),
                city,
                birth_year,
            }
        })
        .collect();

    // --- KNOWS edges ---
    // For each person, sample ~avg_knows_degree distinct other persons. We
    // dedup per source so the effective degree is at most the target.
    let mut knows = Vec::new();
    if params.persons > 1 {
        for src in 0..params.persons {
            let mut targets = std::collections::BTreeSet::new();
            let mut attempts = 0;
            while targets.len() < params.avg_knows_degree && attempts < params.avg_knows_degree * 4
            {
                attempts += 1;
                let t = rng.next_below(params.persons);
                if t != src {
                    targets.insert(t);
                }
            }
            for t in targets {
                knows.push((src, t));
            }
        }
    }

    // --- Forums ---
    let forums: Vec<GenForum> = (0..params.forums)
        .map(|idx| GenForum {
            idx,
            title: format!("Forum{idx}"),
            moderator: rng.next_below(params.persons.max(1)),
        })
        .collect();

    // --- Posts (with deterministic embeddings) ---
    let mut posts = Vec::new();
    let mut post_idx = 0usize;
    for forum in &forums {
        for _ in 0..params.posts_per_forum {
            let creator = rng.next_below(params.persons.max(1));
            let tag = if params.tags > 0 {
                rng.next_below(params.tags)
            } else {
                0
            };
            let length = 40 + rng.next_below(400) as i64;
            let embedding = deterministic_embedding(&mut rng, params.embedding_dim);
            posts.push(GenPost {
                idx: post_idx,
                forum: forum.idx,
                creator,
                tag,
                length,
                embedding,
            });
            post_idx += 1;
        }
    }

    // --- Comments ---
    let mut comments = Vec::new();
    let mut comment_idx = 0usize;
    for post in &posts {
        for _ in 0..params.comments_per_post {
            let creator = rng.next_below(params.persons.max(1));
            comments.push(GenComment {
                idx: comment_idx,
                reply_to_post: post.idx,
                creator,
            });
            comment_idx += 1;
        }
    }

    // --- Update-stream person revisions (bi-temporal history) ---
    // Apply revisions to the first quarter of persons so the temporal
    // extension has multiple versions to reconstruct.
    let mut person_revisions = Vec::new();
    let revised_persons = (params.persons / 4).max(1).min(params.persons);
    for person in 0..revised_persons {
        for revision in 1..=params.update_revisions {
            let city = CITIES[rng.next_below(CITIES.len())].to_string();
            person_revisions.push(GenPersonRevision {
                person,
                revision,
                city,
            });
        }
    }

    GeneratedGraph {
        scale: scale.label().to_string(),
        seed,
        params,
        persons,
        knows,
        forums,
        posts,
        comments,
        tags,
        person_revisions,
    }
}

/// Produce a deterministic unit-ish embedding vector.
fn deterministic_embedding(rng: &mut SplitMix64, dim: usize) -> Vec<f32> {
    let mut v: Vec<f32> = (0..dim).map(|_| rng.next_f32() - 0.5).collect();
    // Normalize to unit length so cosine similarity is well-behaved.
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in &mut v {
            *x /= norm;
        }
    } else if !v.is_empty() {
        v[0] = 1.0;
    }
    v
}
