use crate::{InlineError, Utf16Range};

pub fn slice_utf16_range(value: &str, range: Utf16Range) -> Result<String, InlineError> {
    let units: Vec<u16> = value.encode_utf16().collect();
    let end = validate_range(&units, range)?;
    String::from_utf16(&units[range.location..end]).map_err(|_| InlineError::InvalidRange)
}

pub fn replace_utf16_range(
    value: &str,
    range: Utf16Range,
    replacement: &str,
) -> Result<String, InlineError> {
    let mut units: Vec<u16> = value.encode_utf16().collect();
    let end = validate_range(&units, range)?;
    units.splice(range.location..end, replacement.encode_utf16());
    String::from_utf16(&units).map_err(|_| InlineError::InvalidRange)
}

/// Returns UTF-16 chunks without splitting surrogate pairs.
pub fn utf16_chunks(value: &str, maximum_units: usize) -> Result<Vec<Vec<u16>>, InlineError> {
    if maximum_units == 0 {
        return Err(InlineError::InvalidRange);
    }
    let units: Vec<u16> = value.encode_utf16().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < units.len() {
        let mut end = start.saturating_add(maximum_units).min(units.len());
        if end < units.len() && splits_surrogate_pair(&units, end) {
            end = end.saturating_sub(1);
        }
        if end == start {
            return Err(InlineError::InvalidRange);
        }
        chunks.push(units[start..end].to_vec());
        start = end;
    }
    Ok(chunks)
}

fn validate_range(units: &[u16], range: Utf16Range) -> Result<usize, InlineError> {
    let end = range.end().ok_or(InlineError::InvalidRange)?;
    if end > units.len()
        || splits_surrogate_pair(units, range.location)
        || splits_surrogate_pair(units, end)
    {
        return Err(InlineError::InvalidRange);
    }
    Ok(end)
}

fn splits_surrogate_pair(units: &[u16], index: usize) -> bool {
    index > 0
        && index < units.len()
        && (0xD800..=0xDBFF).contains(&units[index - 1])
        && (0xDC00..=0xDFFF).contains(&units[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_and_replaces_in_utf16_coordinates() {
        let value = "a🐶bc";
        let dog = Utf16Range {
            location: 1,
            length: 2,
        };
        assert_eq!(slice_utf16_range(value, dog).unwrap(), "🐶");
        assert_eq!(
            replace_utf16_range(value, dog, "boxer").unwrap(),
            "aboxerbc"
        );
    }

    #[test]
    fn rejects_ranges_that_split_surrogate_pairs_or_overflow() {
        let value = "a🐶b";
        assert_eq!(
            slice_utf16_range(
                value,
                Utf16Range {
                    location: 2,
                    length: 1
                }
            ),
            Err(InlineError::InvalidRange)
        );
        assert_eq!(
            replace_utf16_range(
                value,
                Utf16Range {
                    location: usize::MAX,
                    length: 2
                },
                ""
            ),
            Err(InlineError::InvalidRange)
        );
    }

    #[test]
    fn chunks_without_splitting_emoji() {
        let chunks = utf16_chunks("ab🐶cd", 3).unwrap();
        assert_eq!(
            chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 3, 1]
        );
        assert_eq!(
            chunks
                .into_iter()
                .map(|chunk| String::from_utf16(&chunk).unwrap())
                .collect::<String>(),
            "ab🐶cd"
        );
    }
}
