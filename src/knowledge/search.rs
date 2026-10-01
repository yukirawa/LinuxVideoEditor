//! Semantic scene ranking.
//!
//! Deliberately tiny: vectors in, ranked scene IDs out. The embedding model's choice does not
//! leak in here, which keeps retrieval independent of whether a local or a network model
//! produced the vectors.

use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq)]
pub struct SceneHit {
    pub scene_id: Uuid,
    pub score: f32,
}

/// Cosine similarity, the standard metric for text embeddings. Returns 0 for mismatched,
/// empty, or zero-length vectors rather than producing NaN.
pub fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.len() != right.len() || left.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut left_norm = 0.0f32;
    let mut right_norm = 0.0f32;
    for (a, b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    let denominator = left_norm.sqrt() * right_norm.sqrt();
    if denominator == 0.0 {
        0.0
    } else {
        dot / denominator
    }
}

/// Ranks stored scene vectors against a query vector, best first. Ties break on scene ID so the
/// order is deterministic.
pub fn rank(query: &[f32], vectors: &BTreeMap<Uuid, Vec<f32>>, limit: usize) -> Vec<SceneHit> {
    let mut hits: Vec<SceneHit> = vectors
        .iter()
        .map(|(scene_id, vector)| SceneHit {
            scene_id: *scene_id,
            score: cosine_similarity(query, vector),
        })
        .filter(|hit| hit.score.is_finite())
        .collect();
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.scene_id.cmp(&right.scene_id))
    });
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    #[test]
    fn cosine_matches_expectations() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn rank_orders_and_limits() {
        let mut vectors = BTreeMap::new();
        vectors.insert(id(1), vec![1.0, 0.0]);
        vectors.insert(id(2), vec![0.0, 1.0]);
        vectors.insert(id(3), vec![0.9, 0.1]);
        let hits = rank(&[1.0, 0.0], &vectors, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].scene_id, id(1));
        assert!(hits[0].score > hits[1].score);
    }
}
