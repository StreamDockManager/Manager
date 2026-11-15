//! Bounded stream mechanics. Path and handle trust policies belong to callers
use std::io::Read;

/// Consume at most `limit + 1` bytes; the extra byte distinguishes exact-size EOF
/// from an oversized document without trusting a racy metadata length.
pub fn read_from(reader: impl Read, limit: usize, too_large: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > limit {
        return Err(too_large.into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify growth after open is bounded without trusting original length
    fn growth_after_open_is_bounded_without_trusting_original_length() {
        use std::io::Write;
        let mut source = tempfile::NamedTempFile::new().unwrap();
        source.write_all(b"1234").unwrap();
        let opened = std::fs::File::open(source.path()).unwrap();
        assert_eq!(opened.metadata().unwrap().len(), 4);
        source.write_all(b"5").unwrap();
        assert_eq!(read_from(opened, 4, "oversize").unwrap_err(), "oversize");
    }
}
