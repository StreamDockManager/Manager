//! Maximum encoded envelope for durable builtin values; live maps are capped at 4 MiB
pub(super) const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;
