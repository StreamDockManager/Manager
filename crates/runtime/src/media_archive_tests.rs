use super::*;
use image::ImageEncoder;
impl Clip {
    // Construct the original archive fixture for media archive tests
    fn original_archive(&self, source: &[u8]) -> Result<Vec<u8>, String> {
        if source.len() > MAX_SOURCE {
            return Err("Media source exceeds 16 MiB".into());
        }
        let mut zip = zip::ZipWriter::new(Cursor::new(vec![]));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6));
        zip.start_file("animation.json", options)
            .map_err(|e| e.to_string())?;
        let metadata = Metadata {
            schema_version: 1,
            delays_us: self.delays_us.clone(),
            preparation_revision: self.preparation_revision,
            clip_info: self.clip_info.clone(),
            has_original: !source.is_empty(),
            source_format: self.source_format.clone(),
            delays_ms: self.delays_ms.clone(),
            frames: self
                .frames
                .iter()
                .map(|frame| {
                    use sha2::{Digest, Sha256};
                    hex::encode(Sha256::digest(&***frame))
                })
                .collect(),
        };
        zip.write_all(&serde_json::to_vec(&metadata).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if !source.is_empty() {
            zip.start_file(format!("original.{}", self.source_format), options)
                .map_err(|e| e.to_string())?;
            zip.write_all(source).map_err(|e| e.to_string())?;
        }
        let mut seen = std::collections::HashSet::new();
        for (index, frame) in self.frames.iter().enumerate() {
            let hash = &metadata.frames[index];
            if !seen.insert(hash) {
                continue;
            }
            zip.start_file(format!("frames/{hash}.image"), options)
                .map_err(|e| e.to_string())?;
            zip.write_all(frame).map_err(|e| e.to_string())?;
        }
        let bytes = zip.finish().map_err(|e| e.to_string())?.into_inner();
        if bytes.len() > MAX_ASSET {
            return Err("Prepared animation exceeds 96 MiB".into());
        }
        Ok(bytes)
    }
}
// Construct an isolated media archive tests fixture
fn fixture() -> Clip {
    let frames = (0..64)
        .map(|index| {
            let pixels = image::RgbImage::from_pixel(32, 32, image::Rgb([index, 100, 150]));
            let mut exif = b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
            exif.resize(64 * 1024, 0);
            let mut bytes = Vec::new();
            let mut encoder = image::codecs::png::PngEncoder::new(&mut bytes);
            encoder.set_exif_metadata(exif).unwrap();
            encoder
                .write_image(&pixels, 32, 32, image::ExtendedColorType::Rgb8)
                .unwrap();
            bytes
        })
        .collect();
    Clip::new("gif", frames, vec![50; 64]).unwrap()
}
#[test]
// Verify that file archives preserve entry bytes with bounded working allocations
fn file_archives_preserve_entry_bytes_with_bounded_working_allocations() {
    let clip = fixture();
    let source = tests::sample_gif();
    let control = PreparationControl::default();
    clip.validate_prepared(&source, &control).unwrap();
    for repetition in 0..5 {
        let (original, old_count, old_bytes) =
            allocation_tests::measure_allocations(|| clip.original_archive(&source).unwrap());
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let ((), new_count, new_bytes) = allocation_tests::measure_allocations(|| {
            clip.write_archive(file.as_file_mut(), &source, &control)
                .unwrap();
        });
        // DEFLATE can encode equivalent bytes differently when JSON is streamed
        // in smaller writes. Compare payloads and ZIP metadata, not compressed bytes
        {
            let mut expected = zip::ZipArchive::new(Cursor::new(&original)).unwrap();
            let mut actual =
                zip::ZipArchive::new(std::fs::File::open(file.path()).unwrap()).unwrap();
            assert_eq!(actual.len(), expected.len());
            for index in 0..expected.len() {
                let mut a = actual.by_index(index).unwrap();
                let mut b = expected.by_index(index).unwrap();
                assert_eq!(a.name().unwrap(), b.name().unwrap());
                assert_eq!(a.compression(), zip::CompressionMethod::Deflated);
                assert_eq!(a.unix_mode(), b.unix_mode());
                let mut left = Vec::new();
                let mut right = Vec::new();
                a.read_to_end(&mut left).unwrap();
                b.read_to_end(&mut right).unwrap();
                assert_eq!(left, right);
            }
        }
        // Both paths now pay the same bounded DEFLATE workspace cost. Compare
        // against the in-memory writer, not the obsolete Stored-only 4x ratio
        assert!(
            new_bytes < old_bytes,
            "file writer should avoid the archive output buffer"
        );
        drop(original);
        let (old_clip, _, old_read_bytes) = allocation_tests::measure_allocations(|| {
            let bytes = std::fs::read(file.path()).unwrap();
            Clip::from_archive_controlled(&bytes, &Default::default()).unwrap()
        });
        let (new_clip, _, new_read_bytes) = allocation_tests::measure_allocations(|| {
            Clip::from_reader_controlled(
                BufReader::new(std::fs::File::open(file.path()).unwrap()),
                &control,
            )
            .unwrap()
        });
        assert_eq!(old_clip.delays_us, new_clip.delays_us);
        assert_eq!(old_clip.frame_digests, new_clip.frame_digests);
        assert!(new_read_bytes < old_read_bytes);
        eprintln!("MEDIA_ARCHIVE repetition={repetition} zip_bytes={} old_write_requested_bytes={old_bytes} new_write_requested_bytes={new_bytes} old_write_allocations={old_count} new_write_allocations={new_count} old_load_requested_bytes={old_read_bytes} new_load_requested_bytes={new_read_bytes}",file.as_file().metadata().unwrap().len());
    }
}
#[test]
// Verify that direct prepared validation retains archive integrity and cancellation checks
fn direct_prepared_validation_retains_archive_integrity_and_cancellation_checks() {
    let source = tests::sample_gif();
    let control = PreparationControl::default();
    let invalid = Clip::new("gif", vec![b"invalid image".to_vec()], vec![50]).unwrap();
    assert!(invalid.validate_prepared(&source, &control).is_err());
    assert!(
        Clip::from_archive_controlled(&invalid.archive(&source).unwrap(), &Default::default())
            .is_err()
    );
    let mut clip = fixture();
    assert!(clip.validate_prepared(b"not a GIF", &control).is_err());
    assert!(clip.validate_prepared(&[], &control).is_err());
    clip.frame_digests[0] = "0".repeat(64);
    assert!(clip
        .validate_prepared(&source, &control)
        .unwrap_err()
        .contains("integrity"));
    control
        .cancelled
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let mut file = tempfile::NamedTempFile::new().unwrap();
    assert!(clip
        .write_archive(file.as_file_mut(), &source, &control)
        .is_err());
    assert_eq!(file.as_file().metadata().unwrap().len(), 0);
    assert!(Clip::from_reader_controlled(Cursor::new(vec![]), &control).is_err());
    let file = tempfile::NamedTempFile::new().unwrap();
    file.as_file().set_len(MAX_ASSET as u64 + 1).unwrap();
    assert!(
        Clip::from_reader_controlled(file.as_file(), &PreparationControl::default())
            .err()
            .unwrap()
            .contains("96 MiB")
    );
}

#[test]
// Verify that direct prepared publication rejects mutated playback timeline
fn direct_prepared_publication_rejects_mutated_playback_timeline() {
    let source = tests::sample_gif();
    for mutation in 0..4 {
        let mut clip = prepare(&source).unwrap();
        match mutation {
            0 => clip.delays_us[0] += 1000,
            1 => clip.delays_ms[0] += 1,
            2 => clip.duration_ms += 1,
            _ => clip.ends_us[0] += 1000,
        }
        assert!(clip
            .validate_prepared(&source, &Default::default())
            .unwrap_err()
            .contains("timeline"));
    }
}
