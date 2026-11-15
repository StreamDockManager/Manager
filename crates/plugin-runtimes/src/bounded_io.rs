//! Bounded configuration reads, including files that grow after being opened
#[cfg(test)]
use std::io::Read;
use std::path::Path;

// Read for the current operation
pub fn read(path: &Path, limit: usize, too_large: &str) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    read_from(file, limit, too_large)
}

pub use stream_dock_manager_storage::bounded_io::read_from;

#[cfg(test)]
mod tests {
    use super::*;
    struct Counted {
        remaining: usize,
        consumed: usize,
        chunk: usize,
    }
    impl Read for Counted {
        // Read for the current operation
        fn read(&mut self, target: &mut [u8]) -> std::io::Result<usize> {
            let size = target.len().min(self.remaining).min(self.chunk);
            target[..size].fill(b' ');
            self.remaining -= size;
            self.consumed += size;
            Ok(size)
        }
    }
    #[test]
    // Verify accepts exact limit and short reads without large allocations
    fn accepts_exact_limit_and_short_reads_without_large_allocations() {
        for size in [0, 15, 16] {
            let mut reader = Counted {
                remaining: size,
                consumed: 0,
                chunk: 3,
            };
            assert_eq!(read_from(&mut reader, 16, "large").unwrap().len(), size);
            assert_eq!(reader.consumed, size);
        }
    }
    #[test]
    // Verify consumes only one excess byte even if source grows without bound
    fn consumes_only_one_excess_byte_even_if_source_grows_without_bound() {
        let mut reader = Counted {
            remaining: usize::MAX,
            consumed: 0,
            chunk: 3,
        };
        assert_eq!(read_from(&mut reader, 16, "large").unwrap_err(), "large");
        assert_eq!(reader.consumed, 17);
    }
    #[test]
    // Verify propagates read failures
    fn propagates_read_failures() {
        struct Broken;
        impl Read for Broken {
            // Read for the current operation
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("read failed"))
            }
        }
        assert!(read_from(Broken, 16, "large")
            .unwrap_err()
            .contains("read failed"));
    }
}
