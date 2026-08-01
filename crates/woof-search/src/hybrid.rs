use std::collections::BTreeMap;

use crate::index::VectorHit;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LexicalHit {
    pub key: u64,
    /// Raw BM25/FTS score retained for consumers; fusion uses rank because
    /// lexical and vector score scales are not directly comparable.
    pub score: f32,
}

pub type RankedVectorHit = VectorHit;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HybridWeights {
    pub lexical: f32,
    pub vector: f32,
    pub reciprocal_rank_constant: usize,
}

impl Default for HybridWeights {
    fn default() -> Self {
        Self {
            lexical: 0.55,
            vector: 0.45,
            reciprocal_rank_constant: 60,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HybridHit {
    pub key: u64,
    pub score: f32,
    pub lexical_rank: Option<usize>,
    pub vector_rank: Option<usize>,
    pub lexical_score: Option<f32>,
    pub vector_distance: Option<f32>,
}

#[derive(Default)]
struct FusionState {
    score: f32,
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    lexical_score: Option<f32>,
    vector_distance: Option<f32>,
}

/// Weighted reciprocal-rank fusion.
///
/// Inputs are expected in best-first order. The first occurrence of a key in
/// each source wins, making duplicate handling deterministic.
pub fn hybrid_rank_merge(
    lexical: &[LexicalHit],
    vector: &[RankedVectorHit],
    limit: usize,
    weights: HybridWeights,
) -> Vec<HybridHit> {
    if limit == 0 {
        return Vec::new();
    }
    let lexical_weight = finite_nonnegative(weights.lexical);
    let vector_weight = finite_nonnegative(weights.vector);
    let rank_constant = weights.reciprocal_rank_constant.max(1) as f32;
    let mut states = BTreeMap::<u64, FusionState>::new();

    for (index, hit) in lexical.iter().enumerate() {
        let rank = index + 1;
        let state = states.entry(hit.key).or_default();
        if state.lexical_rank.is_none() {
            state.lexical_rank = Some(rank);
            state.lexical_score = Some(hit.score);
            state.score += lexical_weight / (rank_constant + rank as f32);
        }
    }
    for (index, hit) in vector.iter().enumerate() {
        let rank = index + 1;
        let state = states.entry(hit.key).or_default();
        if state.vector_rank.is_none() {
            state.vector_rank = Some(rank);
            state.vector_distance = Some(hit.distance);
            state.score += vector_weight / (rank_constant + rank as f32);
        }
    }

    let mut hits: Vec<HybridHit> = states
        .into_iter()
        .map(|(key, state)| HybridHit {
            key,
            score: state.score,
            lexical_rank: state.lexical_rank,
            vector_rank: state.vector_rank,
            lexical_score: state.lexical_score,
            vector_distance: state.vector_distance,
        })
        .collect();
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.key.cmp(&right.key))
    });
    hits.truncate(limit);
    hits
}

fn finite_nonnegative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_candidates_are_promoted_and_ties_are_stable() {
        let lexical = [
            LexicalHit { key: 1, score: 9.0 },
            LexicalHit { key: 2, score: 8.0 },
            LexicalHit { key: 3, score: 7.0 },
        ];
        let vector = [
            VectorHit {
                key: 3,
                distance: 0.1,
            },
            VectorHit {
                key: 4,
                distance: 0.2,
            },
            VectorHit {
                key: 2,
                distance: 0.3,
            },
        ];
        let merged = hybrid_rank_merge(&lexical, &vector, 4, HybridWeights::default());
        assert_eq!(merged[0].key, 3);
        assert_eq!(merged[1].key, 2);
        assert_eq!(merged.len(), 4);
    }

    #[test]
    fn duplicate_source_keys_only_contribute_once() {
        let lexical = [
            LexicalHit { key: 1, score: 3.0 },
            LexicalHit { key: 1, score: 2.0 },
        ];
        let merged = hybrid_rank_merge(&lexical, &[], 10, HybridWeights::default());
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].lexical_rank, Some(1));
    }
}
