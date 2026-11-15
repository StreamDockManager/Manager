//! Inspect raw entry headers before ZIP creates a decoder or allocates a dictionary
use std::io::{Read, Seek};

// Validate lzma headers
pub fn validate_lzma_headers<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<(), String> {
    for index in 0..archive.len() {
        let mut entry = archive.by_index_raw(index).map_err(|e| e.to_string())?;
        if entry.encrypted() {
            return Err("Encrypted or DRM-protected packages are unsupported; obtain an unprotected plugin from its publisher".into());
        }
        if !matches!(
            entry.compression(),
            zip::CompressionMethod::Stored
                | zip::CompressionMethod::Deflated
                | zip::CompressionMethod::Lzma
        ) {
            return Err("Unsupported ZIP compression method".into());
        }
        if entry.compression() == zip::CompressionMethod::Lzma {
            let mut header = [0u8; 9];
            entry
                .read_exact(&mut header)
                .map_err(|_| "Truncated ZIP LZMA properties")?;
            let properties_size = u16::from_le_bytes([header[2], header[3]]);
            let dictionary = u32::from_le_bytes(header[5..9].try_into().unwrap());
            if properties_size != 5 || header[4] >= 225 {
                return Err("Malformed ZIP LZMA properties".into());
            }
            if dictionary > 64 * 1024 * 1024 {
                return Err("ZIP LZMA dictionary exceeds 64 MiB".into());
            }
        }
    }
    Ok(())
}

/// ZIP's decoder accepts a known-size stream without consuming its EOS marker
/// For entries advertising EOS, verify the complete LZMA stream with bounded output
/// before publication, including the marker and actual expanded size.
pub fn validate_lzma_eos<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    max_file: u64,
    max_total: u64,
) -> Result<(), String> {
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index_raw(index).map_err(|e| e.to_string())?;
        let size = entry.size();
        total = total.checked_add(size).ok_or("Archive size overflow")?;
        if size > max_file || total > max_total {
            return Err("Archive exceeds expanded size limits".into());
        }
        if entry.compression() != zip::CompressionMethod::Lzma || entry.flags().as_u16() & 2 == 0 {
            continue;
        }
        let mut properties = [0; 9];
        entry
            .read_exact(&mut properties)
            .map_err(|e| e.to_string())?;
        let dictionary = u32::from_le_bytes(properties[5..9].try_into().unwrap());
        if dictionary > 64 * 1024 * 1024 || properties[4] >= 225 || properties[2..4] != [5, 0] {
            return Err("Invalid LZMA properties".into());
        }
        let decoder = lzma_rust2::LzmaReader::new_with_props(
            entry,
            u64::MAX,
            properties[4],
            dictionary,
            None,
        )
        .map_err(|e| e.to_string())?;
        let actual = std::io::copy(&mut decoder.take(size + 1), &mut std::io::sink())
            .map_err(|e| format!("Corrupt or truncated LZMA stream: {e}"))?;
        if actual != size {
            return Err("LZMA expanded size disagrees with ZIP header".into());
        }
    }
    Ok(())
}
