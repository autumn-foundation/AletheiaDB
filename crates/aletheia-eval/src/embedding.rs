//! Deterministic synthetic embeddings (Issue #3366).
//!
//! The harness must be fully reproducible and must not depend on any external
//! embedding model or API key. So instead of a learned model we use a
//! **feature-hashing vectorizer**: a documented, seeded, pure function from
//! text to a fixed-dimension unit vector. Two entities that share tokens land
//! near each other in cosine space; unrelated entities are near-orthogonal.
//!
//! # Algorithm (reproducible)
//!
//! 1. Lowercase the text and split into tokens on any non-alphanumeric byte.
//! 2. For each token, compute a 64-bit FNV-1a hash of `"{seed}:{token}"`
//!    (FNV-1a, **not** the standard-library `DefaultHasher`, whose seed is
//!    process-randomised — determinism across runs/machines requires a fixed
//!    hash).
//! 3. Map the hash to a bucket `hash % dim` and a sign `±1` from the top bit.
//!    Accumulate the signed unit into that bucket.
//! 4. L2-normalise the accumulated vector (a zero vector — empty text — is
//!    returned as all-zeros).
//!
//! Cosine similarity between two such vectors grows with the number of shared
//! tokens, so a question that names an entity ("Acme") is nearest to that
//! entity's node. This is the only "embedding model" the harness uses.

/// Default embedding dimensionality used by the bundled datasets.
///
/// 256 buckets keep feature-hashing collisions rare enough that a single shared
/// token (e.g. an entity name common to a question and its target) reliably
/// dominates the cosine similarity over collision noise. At 64 dims the noise
/// occasionally overwhelms a lone shared token and mis-ranks the target.
pub const DEFAULT_DIM: usize = 256;

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64-bit hash of `bytes` — a fixed, non-randomised hash so embeddings
/// are byte-identical across processes and machines.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Compute the deterministic synthetic embedding of `text` into `dim`
/// dimensions under `seed`.
///
/// See the module docs for the exact, reproducible algorithm. The returned
/// vector is L2-normalised (or all-zeros for text with no alphanumeric
/// tokens).
#[must_use]
pub fn embed(text: &str, dim: usize, seed: u64) -> Vec<f32> {
    let mut vector = vec![0.0f32; dim.max(1)];
    let dim = vector.len();

    let mut token = String::new();
    let flush = |token: &mut String, vector: &mut [f32]| {
        if token.is_empty() {
            return;
        }
        let keyed = format!("{seed}:{token}");
        let hash = fnv1a(keyed.as_bytes());
        let bucket = (hash % dim as u64) as usize;
        let sign = if hash & (1 << 63) == 0 { 1.0 } else { -1.0 };
        vector[bucket] += sign;
        token.clear();
    };

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            token.extend(ch.to_lowercase());
        } else {
            flush(&mut token, &mut vector);
        }
    }
    flush(&mut token, &mut vector);

    l2_normalize(&mut vector);
    vector
}

fn l2_normalize(vector: &mut [f32]) {
    let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vector.iter_mut() {
            *v /= norm;
        }
    }
}

/// Cosine similarity between two equal-length vectors (0.0 if either has zero
/// norm). Used only in tests and diagnostics; the database computes its own
/// distances during vector search.
#[must_use]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_across_calls() {
        let a = embed("Alice is the CEO of Acme", DEFAULT_DIM, 42);
        let b = embed("Alice is the CEO of Acme", DEFAULT_DIM, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn seed_changes_embedding() {
        let a = embed("Acme", DEFAULT_DIM, 1);
        let b = embed("Acme", DEFAULT_DIM, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn normalized_unit_length() {
        let v = embed("some non trivial text here", DEFAULT_DIM, 7);
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn empty_text_is_zero_vector() {
        let v = embed("   !!!  ", DEFAULT_DIM, 3);
        assert!(v.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn shared_tokens_are_more_similar() {
        // A question naming "Acme" should be closer to the Acme entity than to
        // an unrelated one — the property the whole retrieval harness relies on.
        let question = embed("Who is the CEO of Acme", DEFAULT_DIM, 42);
        let acme = embed("Acme Corporation technology company", DEFAULT_DIM, 42);
        let globex = embed("Globex Industries mining conglomerate", DEFAULT_DIM, 42);
        assert!(
            cosine(&question, &acme) > cosine(&question, &globex),
            "expected Acme to be nearer than Globex"
        );
    }
}
