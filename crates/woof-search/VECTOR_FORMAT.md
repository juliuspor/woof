# woof vector index

woof rebuilds its vector index locally from canonical SQLite records. The
embedding and index pipeline performs no network access and needs no external
model files.

## Embeddings

- 512 dimensions.
- On macOS, mean-pooled word vectors from the built-in Natural Language
  framework, preferring English or the detected language according to bounded
  vocabulary coverage. Native vectors up to 512 components are zero-padded to
  the fixed container width, preserving cosine geometry.
- When compatible system vectors are unavailable, deterministic signed feature
  hashing with SHA-256 domain separation supplies word, adjacent-word, and
  character n-gram features.
- Unit-length vectors for inner-product search.
- The pipeline bounds inputs, token counts, and token lengths before framework
  calls.
- The pipeline doesn't use model payloads, asset requests, downloads, or
  network operations.
- macOS controls built-in vocabulary availability and revisions. An
  unavailable embedding, or one wider than the fixed container, selects the
  lexical fallback.

## Graph container

- USearch 2.25.1 with inner-product distance and f16 scalars.
- HNSW connectivity M=16 and base-layer connectivity M0=32.
- Checksummed `WOOFHNSW` container with canonical record metadata.
- A deterministic probe signature forces a rebuild whenever the available
  system/fallback embedding backend or its vector revision changes.
- Atomic same-directory replacement and required file mode `0600`.
- Strict rejection of malformed metadata, checksum failures, wrong dimensions,
  non-private permissions, and key collisions.

## Vector keys

The pipeline derives keys with SHA-256 over a fixed woof domain, the record
namespace, a NUL separator, and the stable record ID. The first eight digest
bytes form a big-endian unsigned integer; zero maps to one. Rebuilds reject
collisions.

## Hybrid ranking

Weighted reciprocal-rank fusion merges lexical BM25 and vector results
(lexical 0.55, vector 0.45, rank constant 60). The merger deduplicates
candidate keys and orders equal scores by ascending key.
