use sha2::{Digest, Sha256};

use crate::{SearchError, DIMENSIONS};

const MAX_INPUT_CHARACTERS: usize = 16_384;
const MAX_TOKENS: usize = 512;
const MAX_TOKEN_CHARACTERS: usize = 64;

pub trait Embedder: Send + Sync {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, SearchError>;

    fn embed_one(&self, text: &str) -> Result<Vec<f32>, SearchError> {
        let mut embeddings = self.embed_batch(&[text.to_owned()])?;
        if embeddings.len() != 1 {
            return Err(SearchError::EmbeddingCount);
        }
        Ok(embeddings.remove(0))
    }
}

/// Private local embeddings for hybrid search.
///
/// On macOS, this mean-pools word vectors from the system Natural Language
/// framework. If a compatible embedding is unavailable for the detected
/// language, it uses deterministic signed feature hashing. Neither backend
/// needs model files, downloads, network access, or mutable vocabulary.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalEmbedder;

impl LocalEmbedder {
    pub fn new() -> Self {
        Self
    }

    /// Whether this Mac has a compatible built-in English word embedding.
    ///
    /// Other platforms always return `false` and use the lexical fallback.
    pub fn system_embeddings_available(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            natural_language::embedding_for_tokens("dog", &["dog".to_owned()]).is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn embed_text(text: &str) -> Result<Vec<f32>, SearchError> {
        let (normalized, tokens) = prepare_input(text);

        #[cfg(target_os = "macos")]
        if let Some(vector) = natural_language::embedding_for_tokens(&normalized, &tokens) {
            return Ok(vector);
        }

        lexical_embedding(&normalized, &tokens)
    }
}

fn prepare_input(text: &str) -> (String, Vec<String>) {
    let normalized = text
        .chars()
        .flat_map(char::to_lowercase)
        .take(MAX_INPUT_CHARACTERS)
        .collect::<String>();
    let tokens = normalized
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .take(MAX_TOKENS)
        .map(|token| token.chars().take(MAX_TOKEN_CHARACTERS).collect::<String>())
        .collect::<Vec<_>>();
    (normalized, tokens)
}

impl Embedder for LocalEmbedder {
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, SearchError> {
        texts.iter().map(|text| Self::embed_text(text)).collect()
    }
}

fn lexical_embedding(normalized: &str, tokens: &[String]) -> Result<Vec<f32>, SearchError> {
    let mut vector = vec![0.0_f32; DIMENSIONS];
    for token in tokens {
        add_feature(&mut vector, b"word", token.as_bytes(), 1.0);

        let bounded = token.chars().collect::<Vec<_>>();
        for width in 3..=5 {
            if bounded.len() < width {
                continue;
            }
            for start in 0..=bounded.len() - width {
                let feature = bounded[start..start + width].iter().collect::<String>();
                add_feature(&mut vector, b"gram", feature.as_bytes(), 0.22);
            }
        }
    }

    for pair in tokens.windows(2) {
        let feature = format!("{}\u{1f}{}", pair[0], pair[1]);
        add_feature(&mut vector, b"pair", feature.as_bytes(), 0.65);
    }

    if tokens.is_empty() {
        for character in normalized
            .chars()
            .filter(|character| !character.is_whitespace())
        {
            let mut buffer = [0_u8; 4];
            add_feature(
                &mut vector,
                b"char",
                character.encode_utf8(&mut buffer).as_bytes(),
                1.0,
            );
        }
    }

    normalize(&mut vector)?;
    Ok(vector)
}

#[cfg(target_os = "macos")]
mod natural_language {
    use std::collections::BTreeSet;

    use objc2::{
        msg_send,
        rc::{autoreleasepool, Retained},
        runtime::{AnyClass, AnyObject},
    };
    use objc2_foundation::NSString;

    use super::{normalize, DIMENSIONS};

    #[link(name = "NaturalLanguage", kind = "framework")]
    unsafe extern "C" {}

    pub(super) fn embedding_for_tokens(text: &str, tokens: &[String]) -> Option<Vec<f32>> {
        if tokens.is_empty() {
            return None;
        }

        autoreleasepool(|_| {
            let embedding_class = AnyClass::get(c"NLEmbedding")?;
            let english = NSString::from_str("en");
            let mut best = aggregate(embedding_class, &english, tokens);

            if let Some(recognizer_class) = AnyClass::get(c"NLLanguageRecognizer") {
                let input = NSString::from_str(text);
                let detected: Option<Retained<NSString>> =
                    unsafe { msg_send![recognizer_class, dominantLanguageForString: &*input] };
                if let Some(detected) = detected {
                    if let Some(candidate) = aggregate(embedding_class, &detected, tokens) {
                        if best
                            .as_ref()
                            .is_none_or(|(_, coverage)| candidate.1 > *coverage)
                        {
                            best = Some(candidate);
                        }
                    }
                }
            }

            let (mut vector, _) = best?;
            normalize(&mut vector).ok()?;
            Some(vector)
        })
    }

    fn aggregate(
        embedding_class: &AnyClass,
        language: &NSString,
        tokens: &[String],
    ) -> Option<(Vec<f32>, usize)> {
        let embedding: Option<Retained<AnyObject>> =
            unsafe { msg_send![embedding_class, wordEmbeddingForLanguage: language] };
        let embedding = embedding?;
        let dimension: usize = unsafe { msg_send![&*embedding, dimension] };
        if dimension == 0 || dimension > DIMENSIONS {
            return None;
        }

        let mut native_vector = vec![0.0_f32; dimension];
        let mut word_vector = vec![0.0_f32; dimension];
        let mut coverage = 0_usize;
        let mut unique = BTreeSet::new();
        for token in tokens {
            if !unique.insert(token.as_str()) {
                continue;
            }
            word_vector.fill(0.0);
            let word = NSString::from_str(token);
            let copied: bool = unsafe {
                msg_send![&*embedding, getVector: word_vector.as_mut_ptr(), forString: &*word]
            };
            if !copied {
                continue;
            }
            if word_vector.iter().any(|value| !value.is_finite()) {
                return None;
            }
            for (total, value) in native_vector.iter_mut().zip(&word_vector) {
                *total += value;
            }
            coverage += 1;
        }
        if coverage == 0 {
            return None;
        }
        // Apple currently exposes smaller native word vectors (for example,
        // 300 English components). Zero-padding into the fixed 512-component
        // container preserves their cosine geometry exactly while keeping the
        // persisted index format platform-independent.
        let mut vector = vec![0.0_f32; DIMENSIONS];
        vector[..dimension].copy_from_slice(&native_vector);
        Some((vector, coverage))
    }
}

fn add_feature(vector: &mut [f32], domain: &[u8], feature: &[u8], weight: f32) {
    let mut hasher = Sha256::new();
    hasher.update(b"woof.local-embedding.v1\0");
    hasher.update(domain);
    hasher.update(b"\0");
    hasher.update(feature);
    let digest = hasher.finalize();

    for offset in [0_usize, 8] {
        let index = u64::from_le_bytes(
            digest[offset..offset + 8]
                .try_into()
                .expect("SHA-256 slices have fixed width"),
        ) as usize
            % vector.len();
        let sign = if digest[offset + 16] & 1 == 0 {
            1.0
        } else {
            -1.0
        };
        vector[index] += sign * weight;
    }
}

fn normalize(vector: &mut [f32]) -> Result<(), SearchError> {
    validate_embedding_shape(vector)?;
    let magnitude = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude <= f32::EPSILON {
        return Err(SearchError::ZeroEmbedding);
    }
    for value in vector {
        *value /= magnitude;
    }
    Ok(())
}

pub(crate) fn validate_embedding(embedding: &[f32]) -> Result<(), SearchError> {
    validate_embedding_shape(embedding)?;
    if embedding.iter().all(|value| value.abs() <= f32::EPSILON) {
        return Err(SearchError::ZeroEmbedding);
    }
    Ok(())
}

fn validate_embedding_shape(embedding: &[f32]) -> Result<(), SearchError> {
    if embedding.len() != DIMENSIONS {
        return Err(SearchError::Dimensions {
            expected: DIMENSIONS,
            actual: embedding.len(),
        });
    }
    if embedding.iter().any(|value| !value.is_finite()) {
        return Err(SearchError::NonFiniteEmbedding);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_fallback_is_deterministic_normalized_and_bounded() {
        let normalized = "a boxer remembers a morning walk through berlin.";
        let tokens = normalized
            .split(|character: char| !character.is_alphanumeric())
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let first = lexical_embedding(normalized, &tokens).unwrap();
        let second = lexical_embedding(normalized, &tokens).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), DIMENSIONS);
        assert!(first.iter().all(|value| value.is_finite()));
        let norm = first.iter().map(|value| value * value).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "unexpected norm {norm}");
    }

    #[test]
    fn fallback_related_lexical_features_score_above_unrelated_text() {
        let lexical = |text: &str| {
            let tokens = text
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            lexical_embedding(text, &tokens).unwrap()
        };
        let query = lexical("quarterly planning notes");
        let related = lexical("notes for quarterly planning");
        let unrelated = lexical("morning bicycle maintenance");
        assert!(dot(&query, &related) > dot(&query, &unrelated));
    }

    #[test]
    fn local_embedder_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LocalEmbedder>();
    }

    #[test]
    fn input_token_count_and_token_length_are_bounded() {
        let long_token = "a".repeat(MAX_INPUT_CHARACTERS + 100);
        let (normalized, tokens) = prepare_input(&long_token);
        assert_eq!(normalized.chars().count(), MAX_INPUT_CHARACTERS);
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].chars().count(), MAX_TOKEN_CHARACTERS);

        let many_tokens = "word ".repeat(MAX_TOKENS + 100);
        let (_, tokens) = prepare_input(&many_tokens);
        assert_eq!(tokens.len(), MAX_TOKENS);
    }

    #[test]
    fn validates_shape_finiteness_and_nonzero() {
        assert!(validate_embedding(&vec![1.0; DIMENSIONS]).is_ok());
        assert!(matches!(
            validate_embedding(&vec![1.0; DIMENSIONS - 1]),
            Err(SearchError::Dimensions { .. })
        ));
        let mut non_finite = vec![1.0; DIMENSIONS];
        non_finite[10] = f32::NAN;
        assert!(matches!(
            validate_embedding(&non_finite),
            Err(SearchError::NonFiniteEmbedding)
        ));
        assert!(matches!(
            validate_embedding(&vec![0.0; DIMENSIONS]),
            Err(SearchError::ZeroEmbedding)
        ));
    }

    fn dot(left: &[f32], right: &[f32]) -> f32 {
        left.iter()
            .zip(right)
            .map(|(left, right)| left * right)
            .sum()
    }
}
