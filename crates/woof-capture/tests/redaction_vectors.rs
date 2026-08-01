use serde::Deserialize;
use woof_capture::Redactor;

#[derive(Deserialize)]
struct Vector {
    name: String,
    input: String,
    expected: String,
}

#[test]
fn fixture_vectors_match() {
    let vectors: Vec<Vector> =
        serde_json::from_str(include_str!("fixtures/redaction-vectors.json")).unwrap();
    for vector in vectors {
        assert_eq!(
            Redactor::default().redact(&vector.input).text,
            vector.expected,
            "vector {}",
            vector.name
        );
    }
}
