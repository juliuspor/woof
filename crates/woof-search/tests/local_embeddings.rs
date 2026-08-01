use woof_search::{Embedder, LocalEmbedder, DIMENSIONS};

#[test]
fn local_embeddings_are_available_without_files_or_network() {
    let embedding = LocalEmbedder::new()
        .embed_one("A boxer dog remembers a walk through Berlin.")
        .unwrap();
    assert_eq!(embedding.len(), DIMENSIONS);
    assert!(embedding.iter().all(|value| value.is_finite()));
    let norm = embedding
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "unexpected norm {norm}");
}

#[cfg(target_os = "macos")]
#[test]
fn system_word_vectors_rank_canine_closer_to_dog_than_bicycle() {
    let embedder = LocalEmbedder::new();
    if !embedder.system_embeddings_available() {
        return;
    }

    let dog = embedder.embed_one("dog").unwrap();
    let canine = embedder.embed_one("canine").unwrap();
    let bicycle = embedder.embed_one("bicycle").unwrap();
    assert!(dot(&dog, &canine) > dot(&dog, &bicycle));
}

#[cfg(target_os = "macos")]
fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}
