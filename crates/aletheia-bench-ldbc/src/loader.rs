//! Load a [`GeneratedGraph`] into a fresh AletheiaDB instance.
//!
//! The loader maps the SNB-shaped generator output onto AletheiaDB's graph
//! model and applies the update stream at increasing valid times so the
//! temporal extension has real bi-temporal history to reconstruct.
//!
//! ## Node labels and edge types (SNB subset mapping)
//!
//! | Generator entity | AletheiaDB label | Key edges                                   |
//! |------------------|------------------|---------------------------------------------|
//! | Person           | `Person`         | `Person -[:KNOWS]-> Person`                 |
//! | Forum            | `Forum`          | `Forum -[:CONTAINER_OF]-> Post`             |
//! | Post             | `Post`           | `Post -[:HAS_CREATOR]-> Person`, `HAS_TAG`  |
//! | Comment          | `Comment`        | `Comment -[:REPLY_OF]-> Post`, `HAS_CREATOR`|
//! | Tag              | `Tag`            | (target of `HAS_TAG`)                        |
//!
//! Post nodes carry an `embedding` vector property (the vector extension's
//! index target).

use crate::generator::GeneratedGraph;
use aletheiadb::{
    AletheiaDB, DistanceMetric, EdgeId, HnswConfig, NodeId, PropertyMapBuilder, Timestamp, time,
};

/// Seconds in one day.
const DAY_SECS: i64 = 86_400;

/// A loaded graph: the live database plus the id maps and temporal anchors the
/// workloads need.
pub struct LoadedGraph {
    /// The live database instance (owns a tempdir-backed WAL).
    pub db: AletheiaDB,
    /// `Person` node ids, indexed by generator person index.
    pub person_ids: Vec<NodeId>,
    /// `Forum` node ids, indexed by generator forum index.
    pub forum_ids: Vec<NodeId>,
    /// `Post` node ids, indexed by generator post index.
    pub post_ids: Vec<NodeId>,
    /// `Comment` node ids, indexed by generator comment index.
    pub comment_ids: Vec<NodeId>,
    /// `Tag` node ids, indexed by generator tag index.
    pub tag_ids: Vec<NodeId>,
    /// Person indices that received update-stream revisions (have history).
    pub revised_persons: Vec<usize>,
    /// The base valid time all initial facts were recorded at (in the past).
    pub base_ts: Timestamp,
    /// A valid time after creation but before any revision (reconstructs the
    /// original state).
    pub early_ts: Timestamp,
    /// The embedding dimensionality.
    pub embedding_dim: usize,
    /// A representative query embedding (a copy of the first post's embedding)
    /// for the vector extension.
    pub query_embedding: Vec<f32>,
    /// Total nodes loaded.
    pub node_count: usize,
    /// Total edges loaded.
    pub edge_count: usize,
    /// Number of embedding vectors indexed (== number of posts).
    pub vector_count: usize,
}

/// Load the generated graph into a new database.
///
/// # Errors
///
/// Returns any error surfaced by the underlying database write/index calls.
pub fn load(graph: &GeneratedGraph) -> aletheiadb::Result<LoadedGraph> {
    let db = AletheiaDB::new()?;
    let dim = graph.params.embedding_dim;

    // Enable the vector index on `embedding` BEFORE inserting posts so every
    // post is indexed on write (the vector extension's k-NN target).
    db.vector_index("embedding")
        .hnsw(HnswConfig::new(dim, DistanceMetric::Cosine))
        .enable()?;

    // Anchor all history comfortably in the past so update-stream revisions
    // stay strictly after creation and strictly before "now".
    let now_secs = time::to_secs(time::now());
    let base_secs = now_secs - 400 * DAY_SECS;
    let base_ts = time::from_secs(base_secs);
    let early_ts = time::from_secs(base_secs + DAY_SECS); // 1 day after creation

    let mut node_count = 0usize;
    let mut edge_count = 0usize;

    // --- Tags ---
    let mut tag_ids = Vec::with_capacity(graph.tags.len());
    for name in &graph.tags {
        let id = db.create_node_with_valid_time(
            "Tag",
            PropertyMapBuilder::new()
                .insert("name", name.as_str())
                .build(),
            Some(base_ts),
        )?;
        tag_ids.push(id);
        node_count += 1;
    }

    // --- Persons ---
    let mut person_ids = Vec::with_capacity(graph.persons.len());
    for p in &graph.persons {
        let id = db.create_node_with_valid_time(
            "Person",
            PropertyMapBuilder::new()
                .insert("name", p.first_name.as_str())
                .insert("city", p.city.as_str())
                .insert("birth_year", p.birth_year)
                .build(),
            Some(base_ts),
        )?;
        person_ids.push(id);
        node_count += 1;
    }

    // --- KNOWS edges ---
    for &(src, dst) in &graph.knows {
        db.create_edge_with_valid_time(
            person_ids[src],
            person_ids[dst],
            "KNOWS",
            PropertyMapBuilder::new().build(),
            Some(base_ts),
        )?;
        edge_count += 1;
    }

    // --- Forums ---
    let mut forum_ids = Vec::with_capacity(graph.forums.len());
    for f in &graph.forums {
        let id = db.create_node_with_valid_time(
            "Forum",
            PropertyMapBuilder::new()
                .insert("title", f.title.as_str())
                .build(),
            Some(base_ts),
        )?;
        forum_ids.push(id);
        node_count += 1;
        // Forum moderator edge.
        if !person_ids.is_empty() {
            db.create_edge_with_valid_time(
                id,
                person_ids[f.moderator.min(person_ids.len() - 1)],
                "HAS_MODERATOR",
                PropertyMapBuilder::new().build(),
                Some(base_ts),
            )?;
            edge_count += 1;
        }
    }

    // --- Posts (with embeddings) ---
    let mut post_ids = Vec::with_capacity(graph.posts.len());
    for post in &graph.posts {
        let id = db.create_node_with_valid_time(
            "Post",
            PropertyMapBuilder::new()
                .insert("length", post.length)
                .insert_vector("embedding", &post.embedding)
                .build(),
            Some(base_ts),
        )?;
        post_ids.push(id);
        node_count += 1;

        // Forum -[:CONTAINER_OF]-> Post
        db.create_edge_with_valid_time(
            forum_ids[post.forum],
            id,
            "CONTAINER_OF",
            PropertyMapBuilder::new().build(),
            Some(base_ts),
        )?;
        edge_count += 1;

        // Post -[:HAS_CREATOR]-> Person
        if !person_ids.is_empty() {
            db.create_edge_with_valid_time(
                id,
                person_ids[post.creator.min(person_ids.len() - 1)],
                "HAS_CREATOR",
                PropertyMapBuilder::new().build(),
                Some(base_ts),
            )?;
            edge_count += 1;
        }

        // Post -[:HAS_TAG]-> Tag
        if !tag_ids.is_empty() {
            db.create_edge_with_valid_time(
                id,
                tag_ids[post.tag.min(tag_ids.len() - 1)],
                "HAS_TAG",
                PropertyMapBuilder::new().build(),
                Some(base_ts),
            )?;
            edge_count += 1;
        }
    }

    // --- Comments ---
    let mut comment_ids = Vec::with_capacity(graph.comments.len());
    for c in &graph.comments {
        let id = db.create_node_with_valid_time(
            "Comment",
            PropertyMapBuilder::new().build(),
            Some(base_ts),
        )?;
        comment_ids.push(id);
        node_count += 1;

        // Comment -[:REPLY_OF]-> Post
        db.create_edge_with_valid_time(
            id,
            post_ids[c.reply_to_post],
            "REPLY_OF",
            PropertyMapBuilder::new().build(),
            Some(base_ts),
        )?;
        edge_count += 1;

        // Comment -[:HAS_CREATOR]-> Person
        if !person_ids.is_empty() {
            db.create_edge_with_valid_time(
                id,
                person_ids[c.creator.min(person_ids.len() - 1)],
                "HAS_CREATOR",
                PropertyMapBuilder::new().build(),
                Some(base_ts),
            )?;
            edge_count += 1;
        }
    }

    // --- Update stream: person revisions at increasing valid times ---
    // Revision r is recorded at base + (r * 10) days, strictly after creation
    // and before "now". This builds the bi-temporal history the temporal
    // extension reconstructs.
    for rev in &graph.person_revisions {
        let valid = time::from_secs(base_secs + (rev.revision as i64) * 10 * DAY_SECS);
        db.update_node_with_valid_time(
            person_ids[rev.person],
            PropertyMapBuilder::new()
                .insert("city", rev.city.as_str())
                .build(),
            Some(valid),
        )?;
    }

    let revised_persons: Vec<usize> = {
        let mut set: Vec<usize> = graph.person_revisions.iter().map(|r| r.person).collect();
        set.sort_unstable();
        set.dedup();
        set
    };

    // --- Dedicated standalone vector-extension corpus (Issue #3628) ---
    // Present only when a `--vector-count` override dialed one in; indexed on
    // the same `embedding` property so the k-NN workload scales over posts +
    // corpus. Each is a lightweight `Embedding` node.
    for v in &graph.vectors {
        db.create_node_with_valid_time(
            "Embedding",
            PropertyMapBuilder::new()
                .insert("corpus_idx", v.idx as i64)
                .insert_vector("embedding", &v.embedding)
                .build(),
            Some(base_ts),
        )?;
        node_count += 1;
    }

    let query_embedding = graph
        .posts
        .first()
        .map(|p| p.embedding.clone())
        .or_else(|| graph.vectors.first().map(|v| v.embedding.clone()))
        .unwrap_or_else(|| vec![0.0; dim]);
    // Honest count of vectors actually indexed: post embeddings plus the
    // dedicated corpus.
    let vector_count = post_ids.len() + graph.vectors.len();

    Ok(LoadedGraph {
        db,
        person_ids,
        forum_ids,
        post_ids,
        comment_ids,
        tag_ids,
        revised_persons,
        base_ts,
        early_ts,
        embedding_dim: dim,
        query_embedding,
        node_count,
        edge_count,
        vector_count,
    })
}

/// Resolve an edge's target node id (thin wrapper used by workloads).
///
/// # Errors
/// Propagates the underlying lookup error.
pub fn edge_target(db: &AletheiaDB, edge: EdgeId) -> aletheiadb::Result<NodeId> {
    db.get_edge_target(edge)
}
