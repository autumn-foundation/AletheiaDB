//! Official LDBC SNB Datagen CSV ingest front-end (Issue #3628).
//!
//! [`ingest`] reads an **official LDBC SNB Datagen** output directory in the
//! `composite-merged-fk` dynamic-entity CSV format (`|`-delimited, one header
//! row per file) and returns a [`GeneratedGraph`] — the exact same struct the
//! built-in synthetic [`crate::generator`] produces. The existing
//! [`crate::loader`], [`crate::runner`], and report path are therefore reused
//! **unchanged**: only the front of the pipeline (where the graph comes from)
//! differs.
//!
//! ## What is ingested (the honest subset)
//!
//! The suite's 15 operations only touch a subset of the SNB schema, so the
//! ingest reads exactly the files needed to populate that subset:
//!
//! | File                          | Populates                                   |
//! |-------------------------------|---------------------------------------------|
//! | `Person.csv`                  | `Person` nodes (`id`, `firstName`)          |
//! | `Person_knows_Person.csv`     | `KNOWS` edges                               |
//! | `Forum.csv`                   | `Forum` nodes (`id`, `title`, moderator FK) |
//! | `Forum_containerOf_Post.csv`  | `CONTAINER_OF` (post → forum)               |
//! | `Post.csv`                    | `Post` nodes (`length`, `CreatorPersonId`)  |
//! | `Comment.csv`                 | `Comment` nodes (`CreatorPersonId`)         |
//! | `Comment_replyOf_Post.csv`    | `REPLY_OF` (comment → post) + `HAS_CREATOR` |
//! | `Tag.csv`                     | `Tag` nodes (target of `HAS_TAG`)           |
//!
//! Large 64-bit LDBC ids are mapped to the harness's dense internal index space
//! via a per-entity-type `HashMap<i64, usize>` (ids are **not** assumed
//! contiguous). Columns are located by **header name** (case-insensitive), never
//! by position, because Datagen column order varies by version. A single
//! trailing empty field and CRLF line endings are tolerated.
//!
//! ## Deliberate limitations (documented, not fudged)
//!
//! * **Embeddings are synthetic.** Official Datagen ships **no** vectors. The
//!   shared [`crate::loader`] mandates a `Post.embedding` vector index, so each
//!   ingested post is given a deterministic *synthetic* embedding (via the same
//!   [`crate::generator`] vector path). The optional `--vector-count` /
//!   `--vector-dim` overrides layer an additional standalone synthetic
//!   `Embedding` corpus on top of the real SNB graph; with no override that
//!   standalone corpus is empty (`vectors` is empty). No embedding value here
//!   originates from Datagen — the graph is real, the vectors are not.
//! * **No update/delete stream.** A static Datagen dump has no revisions, so
//!   `person_revisions` is empty and the temporal-reconstruction extension op
//!   has no revised persons to reconstruct (it no-ops; reported honestly).
//! * **`Comment_replyOf_Comment.csv` is not mapped.** The loader models only
//!   `Comment -[:REPLY_OF]-> Post`; comments replying to another comment are
//!   skipped (their `HAS_CREATOR` edge is therefore also skipped).
//! * **`Person_hasInterest_Tag.csv`, `Post_hasTag_Tag.csv`** and every other SNB
//!   entity/edge (Place, Organisation, TagClass, `Forum_hasMember_Person`,
//!   `Person_likes_*`, `workAt`/`studyAt`, …) are **not** ingested: no suite
//!   operation traverses them. `Post.tag` defaults to the first tag purely so
//!   the loader's (untraversed) `HAS_TAG` edge has a valid target.

use crate::generator::{
    GenComment, GenConfigError, GenForum, GenParams, GenPerson, GenPost, GenVector, GeneratedGraph,
    deterministic_embedding,
};
use crate::prng::SplitMix64;
use std::collections::HashMap;
use std::path::Path;

/// Default embedding dimensionality for the synthetic vector corpus when no
/// `--vector-dim` override is supplied.
const DEFAULT_DATAGEN_DIM: usize = 64;

/// Fixed seed for the deterministic synthetic embedding corpus layered on an
/// ingested Datagen graph (the graph itself is real; only vectors are seeded).
const DATAGEN_VECTOR_SEED: u64 = 0xA1E7_1A5E_D0D0_600D;

/// An error encountered while ingesting an LDBC SNB Datagen CSV directory.
#[derive(Debug)]
pub enum DatagenError {
    /// A required CSV file was not present in the directory.
    MissingFile {
        /// The file name that was expected (e.g. `"Person.csv"`).
        file: String,
    },
    /// An I/O error occurred reading a file.
    Io {
        /// The file being read.
        file: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// A file was present but had no header row.
    EmptyFile {
        /// The offending file.
        file: String,
    },
    /// A required column was absent from a file's header row.
    MissingColumn {
        /// The file whose header was searched.
        file: String,
        /// The column (or first candidate name) that was required.
        column: String,
    },
    /// A data row was malformed (wrong field count, or a non-integer id).
    BadRow {
        /// The file containing the row.
        file: String,
        /// The 1-based line number within the file (header is line 1).
        line: usize,
        /// A human description of what was wrong.
        reason: String,
    },
    /// The ingested data is internally inconsistent for the loader (e.g. posts
    /// exist but no forum was ingested to contain them).
    Inconsistent {
        /// A human description of the inconsistency.
        reason: String,
    },
    /// An invalid vector-corpus override was supplied (e.g. a zero dimension).
    Config(GenConfigError),
}

impl std::fmt::Display for DatagenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingFile { file } => {
                write!(f, "required Datagen file `{file}` is missing")
            }
            Self::Io { file, source } => write!(f, "I/O error reading `{file}`: {source}"),
            Self::EmptyFile { file } => write!(f, "Datagen file `{file}` has no header row"),
            Self::MissingColumn { file, column } => {
                write!(
                    f,
                    "Datagen file `{file}` is missing required column `{column}`"
                )
            }
            Self::BadRow { file, line, reason } => {
                write!(f, "malformed row in `{file}` at line {line}: {reason}")
            }
            Self::Inconsistent { reason } => write!(f, "inconsistent Datagen input: {reason}"),
            Self::Config(e) => write!(f, "invalid vector-corpus override: {e}"),
        }
    }
}

impl std::error::Error for DatagenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Config(e) => Some(e),
            _ => None,
        }
    }
}

impl From<GenConfigError> for DatagenError {
    fn from(e: GenConfigError) -> Self {
        Self::Config(e)
    }
}

/// A parsed `|`-delimited CSV file: a header row plus data rows, each row
/// tagged with its 1-based line number for precise error reporting.
struct Table {
    /// The file name this table came from (for error messages).
    file: String,
    /// Lower-cased header column names (for case-insensitive lookup).
    headers: Vec<String>,
    /// Data rows as `(line_number, fields)`.
    rows: Vec<(usize, Vec<String>)>,
}

impl Table {
    /// Locate a column index by trying each candidate header name in order
    /// (case-insensitive). Returns `None` if no candidate matches.
    fn col(&self, candidates: &[&str]) -> Option<usize> {
        for cand in candidates {
            let lc = cand.to_ascii_lowercase();
            if let Some(idx) = self.headers.iter().position(|h| *h == lc) {
                return Some(idx);
            }
        }
        None
    }

    /// Like [`Table::col`] but returns a [`DatagenError::MissingColumn`] if no
    /// candidate matches.
    fn require_col(&self, candidates: &[&str]) -> Result<usize, DatagenError> {
        self.col(candidates)
            .ok_or_else(|| DatagenError::MissingColumn {
                file: self.file.clone(),
                column: candidates.first().copied().unwrap_or("").to_string(),
            })
    }

    /// Read a field from a row by column index, returning `""` if absent.
    fn field(row: &[String], idx: usize) -> &str {
        row.get(idx).map(String::as_str).unwrap_or("")
    }

    /// Parse an integer id field, mapping failure to [`DatagenError::BadRow`].
    fn parse_id(&self, row: &[String], idx: usize, line: usize) -> Result<i64, DatagenError> {
        let raw = Self::field(row, idx).trim();
        raw.parse::<i64>().map_err(|_| DatagenError::BadRow {
            file: self.file.clone(),
            line,
            reason: format!("expected integer id, found `{raw}`"),
        })
    }
}

/// Read and parse a CSV file. Returns `Ok(None)` when `required` is `false` and
/// the file is absent; returns [`DatagenError::MissingFile`] when it is absent
/// but required.
fn read_table(dir: &Path, name: &str, required: bool) -> Result<Option<Table>, DatagenError> {
    let path = dir.join(name);
    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if required {
                return Err(DatagenError::MissingFile {
                    file: name.to_string(),
                });
            }
            return Ok(None);
        }
        Err(e) => {
            return Err(DatagenError::Io {
                file: name.to_string(),
                source: e,
            });
        }
    };

    let mut lines = contents.lines().enumerate();
    // First line is the header row.
    let Some((_, header_line)) = lines.next() else {
        return Err(DatagenError::EmptyFile {
            file: name.to_string(),
        });
    };
    let headers: Vec<String> = split_row(header_line)
        .into_iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();
    if headers.is_empty() {
        return Err(DatagenError::EmptyFile {
            file: name.to_string(),
        });
    }

    let mut rows = Vec::new();
    for (i, line) in lines {
        // `enumerate` is 0-based over `lines()`; +1 makes it a 1-based file line.
        let line_no = i + 1;
        // Skip blank lines (a trailing newline yields none, but be defensive).
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = split_row(line);
        // Tolerate a single trailing empty field (e.g. a line ending in `|`).
        if fields.len() == headers.len() + 1 && fields.last().is_some_and(|s| s.is_empty()) {
            fields.pop();
        }
        if fields.len() != headers.len() {
            return Err(DatagenError::BadRow {
                file: name.to_string(),
                line: line_no,
                reason: format!("expected {} fields, found {}", headers.len(), fields.len()),
            });
        }
        rows.push((line_no, fields));
    }

    Ok(Some(Table {
        file: name.to_string(),
        headers,
        rows,
    }))
}

/// Split a `|`-delimited row, stripping a trailing `\r` (CRLF tolerance).
fn split_row(line: &str) -> Vec<String> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    line.split('|').map(str::to_string).collect()
}

/// Ingest an official LDBC SNB Datagen CSV output directory into a
/// [`GeneratedGraph`].
///
/// `vector_count` / `vector_dim` size the **synthetic** vector corpus layered on
/// the real SNB graph (Datagen ships no embeddings — see the module docs).
/// `vector_dim` also sizes the (synthetic) per-post embeddings the shared loader
/// requires; `None` uses [`DEFAULT_DATAGEN_DIM`].
///
/// # Errors
///
/// Returns a [`DatagenError`] if a required file/column is missing, a row is
/// malformed, the input is internally inconsistent for the loader, or a
/// `vector_dim` of `Some(0)` is supplied.
pub fn ingest(
    dir: &Path,
    vector_count: Option<usize>,
    vector_dim: Option<usize>,
) -> Result<GeneratedGraph, DatagenError> {
    let dim = match vector_dim {
        Some(0) => return Err(DatagenError::Config(GenConfigError::ZeroVectorDim)),
        Some(d) => d,
        None => DEFAULT_DATAGEN_DIM,
    };

    // --- Persons (required) ---
    let person_tbl =
        read_table(dir, "Person.csv", true)?.ok_or_else(|| DatagenError::MissingFile {
            file: "Person.csv".to_string(),
        })?;
    let p_id = person_tbl.require_col(&["id"])?;
    let p_first = person_tbl.col(&["firstName", "first_name", "name"]);
    let p_birthday = person_tbl.col(&["birthday"]);
    let mut person_map: HashMap<i64, usize> = HashMap::new();
    let mut persons: Vec<GenPerson> = Vec::new();
    for (line, row) in &person_tbl.rows {
        let id = person_tbl.parse_id(row, p_id, *line)?;
        if person_map.contains_key(&id) {
            continue; // first-seen wins; tolerate duplicate id rows
        }
        let idx = persons.len();
        person_map.insert(id, idx);
        let first_name = p_first
            .map(|c| Table::field(row, c).to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("Person{idx}"));
        let birth_year = p_birthday
            .and_then(|c| {
                Table::field(row, c)
                    .get(0..4)
                    .and_then(|y| y.parse::<i64>().ok())
            })
            .unwrap_or(0);
        persons.push(GenPerson {
            idx,
            first_name,
            city: "Unknown".to_string(),
            birth_year,
        });
    }

    // --- KNOWS edges ---
    let mut knows: Vec<(usize, usize)> = Vec::new();
    if let Some(t) = read_table(dir, "Person_knows_Person.csv", false)? {
        let c1 = t.require_col(&["Person1Id", "Person1.id", "person1id"])?;
        let c2 = t.require_col(&["Person2Id", "Person2.id", "person2id"])?;
        for (line, row) in &t.rows {
            let a = t.parse_id(row, c1, *line)?;
            let b = t.parse_id(row, c2, *line)?;
            if let (Some(&sa), Some(&sb)) = (person_map.get(&a), person_map.get(&b)) {
                knows.push((sa, sb)); // endpoints outside the person set are skipped
            }
        }
    }

    // --- Forums ---
    let mut forum_map: HashMap<i64, usize> = HashMap::new();
    let mut forums: Vec<GenForum> = Vec::new();
    if let Some(t) = read_table(dir, "Forum.csv", false)? {
        let f_id = t.require_col(&["id"])?;
        let f_title = t.col(&["title"]);
        let f_mod = t.col(&["ModeratorPersonId", "moderatorPersonId", "moderator"]);
        for (line, row) in &t.rows {
            let id = t.parse_id(row, f_id, *line)?;
            if forum_map.contains_key(&id) {
                continue;
            }
            let idx = forums.len();
            forum_map.insert(id, idx);
            let title = f_title
                .map(|c| Table::field(row, c).to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Forum{idx}"));
            let moderator = f_mod
                .and_then(|c| Table::field(row, c).trim().parse::<i64>().ok())
                .and_then(|pid| person_map.get(&pid).copied())
                .unwrap_or(0);
            forums.push(GenForum {
                idx,
                title,
                moderator,
            });
        }
    }

    // --- Post → Forum FK (from the containerOf relationship file) ---
    let mut post_forum_fk: HashMap<i64, i64> = HashMap::new();
    if let Some(t) = read_table(dir, "Forum_containerOf_Post.csv", false)? {
        let fc = t.require_col(&["ForumId", "Forum.id", "forumid"])?;
        let pc = t.require_col(&["PostId", "Post.id", "postid"])?;
        for (line, row) in &t.rows {
            let fid = t.parse_id(row, fc, *line)?;
            let pid = t.parse_id(row, pc, *line)?;
            post_forum_fk.insert(pid, fid);
        }
    }

    // --- Posts (with synthetic embeddings) ---
    let mut rng = SplitMix64::new(DATAGEN_VECTOR_SEED);
    let mut post_map: HashMap<i64, usize> = HashMap::new();
    let mut posts: Vec<GenPost> = Vec::new();
    if let Some(t) = read_table(dir, "Post.csv", false)? {
        let po_id = t.require_col(&["id"])?;
        let po_len = t.col(&["length"]);
        let po_creator = t.col(&["CreatorPersonId", "creatorpersonid"]);
        let po_forum = t.col(&["ContainerForumId", "containerforumid", "ForumId"]);
        for (line, row) in &t.rows {
            let id = t.parse_id(row, po_id, *line)?;
            if post_map.contains_key(&id) {
                continue;
            }
            let idx = posts.len();
            post_map.insert(id, idx);
            let length = po_len
                .and_then(|c| Table::field(row, c).trim().parse::<i64>().ok())
                .unwrap_or(0);
            let creator = po_creator
                .and_then(|c| Table::field(row, c).trim().parse::<i64>().ok())
                .and_then(|pid| person_map.get(&pid).copied())
                .unwrap_or(0);
            let forum_fk = post_forum_fk
                .get(&id)
                .copied()
                .or_else(|| po_forum.and_then(|c| Table::field(row, c).trim().parse::<i64>().ok()));
            let forum = forum_fk
                .and_then(|fid| forum_map.get(&fid).copied())
                .unwrap_or(0);
            let embedding = deterministic_embedding(&mut rng, dim);
            posts.push(GenPost {
                idx,
                forum,
                creator,
                tag: 0,
                length,
                embedding,
            });
        }
    }

    // The loader indexes `forum_ids[post.forum]`, so posts require a forum to
    // contain them. Fail loudly rather than let the loader panic.
    if !posts.is_empty() && forums.is_empty() {
        return Err(DatagenError::Inconsistent {
            reason: "Post.csv has rows but no Forum was ingested to contain them".to_string(),
        });
    }

    // --- Comment → Post reply FK ---
    let mut comment_post_fk: HashMap<i64, i64> = HashMap::new();
    if let Some(t) = read_table(dir, "Comment_replyOf_Post.csv", false)? {
        let cc = t.require_col(&["CommentId", "Comment.id", "commentid"])?;
        let pc = t.require_col(&["PostId", "Post.id", "ParentPostId", "postid"])?;
        for (line, row) in &t.rows {
            let cid = t.parse_id(row, cc, *line)?;
            let pid = t.parse_id(row, pc, *line)?;
            comment_post_fk.insert(cid, pid);
        }
    }

    // --- Comments (only those replying to a known Post; see module docs) ---
    let mut comments: Vec<GenComment> = Vec::new();
    if let Some(t) = read_table(dir, "Comment.csv", false)? {
        let co_id = t.require_col(&["id"])?;
        let co_creator = t.col(&["CreatorPersonId", "creatorpersonid"]);
        for (line, row) in &t.rows {
            let id = t.parse_id(row, co_id, *line)?;
            // Resolve the reply target to a dense Post index; skip comments that
            // reply to another comment or to an unknown post.
            let Some(post_fk) = comment_post_fk.get(&id).copied() else {
                continue;
            };
            let Some(reply_to_post) = post_map.get(&post_fk).copied() else {
                continue;
            };
            let creator = co_creator
                .and_then(|c| Table::field(row, c).trim().parse::<i64>().ok())
                .and_then(|pid| person_map.get(&pid).copied())
                .unwrap_or(0);
            let idx = comments.len();
            comments.push(GenComment {
                idx,
                reply_to_post,
                creator,
            });
        }
    }

    // --- Tags ---
    let mut tags: Vec<String> = Vec::new();
    if let Some(t) = read_table(dir, "Tag.csv", false)? {
        let t_id = t.require_col(&["id"])?;
        let t_name = t.col(&["name"]);
        for (line, row) in &t.rows {
            let _ = t.parse_id(row, t_id, *line)?; // validate the id column
            let name = t_name
                .map(|c| Table::field(row, c).to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("Tag{}", tags.len()));
            tags.push(name);
        }
    }

    // --- Synthetic standalone vector corpus (empty unless --vector-count) ---
    let vector_count = vector_count.unwrap_or(0);
    let vectors: Vec<GenVector> = (0..vector_count)
        .map(|idx| GenVector {
            idx,
            embedding: deterministic_embedding(&mut rng, dim),
        })
        .collect();

    let params = GenParams {
        persons: persons.len(),
        avg_knows_degree: 0,
        forums: forums.len(),
        posts_per_forum: 0,
        comments_per_post: 0,
        tags: tags.len(),
        embedding_dim: dim,
        update_revisions: 0,
        vector_count,
    };

    Ok(GeneratedGraph {
        scale: "datagen".to_string(),
        seed: DATAGEN_VECTOR_SEED,
        params,
        persons,
        knows,
        forums,
        posts,
        comments,
        tags,
        person_revisions: Vec::new(),
        vectors,
    })
}
