//! Reviewed cleanup of recording sources. Prepared clips are always portable
use crate::{video::Recording, Runtime};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::Path,
};
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema)]
pub struct RecordingCleanupReview {
    pub id: String,
    pub generation: String,
    pub recordings: Vec<Recording>,
    #[ts(type = "number")]
    pub bytes: u64,
}
// Recognize an absent recording file while rejecting unsafe nonregular cleanup targets
fn regular(path: &Path) -> Result<bool, String> {
    match path.symlink_metadata() {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(true),
        Ok(_) => Err("Recording cleanup encountered an unsafe file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}
// Recover the selected video recordings state
pub(crate) fn recover(directory: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .take(16385)
    {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_name().to_string_lossy().starts_with(".cleanup-") {
            continue;
        }
        let root = entry.path();
        let meta = root.symlink_metadata().map_err(|e| e.to_string())?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err("Unsafe recording cleanup journal".into());
        }
        let ids: Vec<String> = serde_json::from_slice(&crate::media_library::read_regular(
            &root.join("review.json"),
            16384,
        )?)
        .map_err(|e| e.to_string())?;
        if ids.len() > 16 {
            return Err("Recording cleanup journal exceeds bounds".into());
        }
        for id in &ids {
            crate::profiles::validate_asset_id(id)?;
        }
        let committed = regular(&root.join("COMMITTED"))?;
        let allowed: BTreeSet<_> = ids
            .iter()
            .flat_map(|id| {
                [
                    format!("{id}.mp4"),
                    format!("{id}.gif"),
                    format!("{id}.json"),
                ]
            })
            .chain(["review.json".into(), "COMMITTED".into()])
            .collect();
        for file in std::fs::read_dir(&root).map_err(|e| e.to_string())? {
            let file = file.map_err(|e| e.to_string())?;
            if !allowed.contains(&file.file_name().to_string_lossy().to_string())
                || !regular(&file.path())?
            {
                return Err("Unexpected recording cleanup journal contents".into());
            }
        }
        for id in &ids {
            if regular(&root.join(format!("{id}.mp4")))?
                && regular(&root.join(format!("{id}.gif")))?
            {
                return Err("Ambiguous recording cleanup sources; files were preserved".into());
            }
            for extension in ["mp4", "gif", "json"] {
                let name = format!("{id}.{extension}");
                let staged = root.join(&name);
                if !regular(&staged)? {
                    continue;
                }
                if committed {
                    std::fs::remove_file(staged).map_err(|e| e.to_string())?;
                } else {
                    let destination = directory.join(name);
                    if destination.exists() {
                        return Err("Recording cleanup recovery conflicts with an existing source; both files were preserved".into());
                    }
                    std::fs::rename(staged, destination).map_err(|e| e.to_string())?;
                }
            }
        }
        std::fs::remove_file(root.join("review.json")).map_err(|e| e.to_string())?;
        if committed {
            std::fs::remove_file(root.join("COMMITTED")).map_err(|e| e.to_string())?;
        }
        std::fs::remove_dir(root).map_err(|e| e.to_string())?;
    }
    Ok(())
}
impl Runtime {
    // Find unreferenced recordings while respecting live leases and clips that retain their source hash
    fn unused_recordings(&self) -> Result<Vec<Recording>, String> {
        let mut referenced = BTreeSet::new();
        let pins = self.video.recording_leases.lock();
        for (_, ids) in pins.live.values() {
            referenced.extend(ids.iter().cloned());
        }
        drop(pins);
        let mut hashes = BTreeSet::new();
        for clip in self.video_clips(None)? {
            hashes.insert(clip.info.recording.sha256);
            referenced.insert(clip.info.recording.id);
        }
        let directory = self.recording_directory()?;
        let mut unused = vec![];
        for (index, entry) in std::fs::read_dir(&directory)
            .map_err(|e| e.to_string())?
            .enumerate()
        {
            if index >= 16384 {
                return Err("Recording scan exceeds bounds; cleanup stopped".into());
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|id| id.to_str()) else {
                continue;
            };
            crate::profiles::validate_asset_id(id)?;
            if referenced.contains(id) {
                continue;
            }
            let recording = self.video_recording(id)?;
            self.recording_path(id)?;
            if hashes.contains(&recording.sha256) {
                continue;
            }
            unused.push(recording);
        }
        unused.sort_by(|a, b| a.id.cmp(&b.id));
        unused.truncate(16);
        Ok(unused)
    }
    // Create a single generation-bound cleanup review of currently unused recordings
    pub fn review_unused_recordings(&self) -> Result<RecordingCleanupReview, String> {
        let _writer = self.video.writer.lock().unwrap();
        let _media = self.profiles.media_gate.exclusive()?;
        let recordings = self.unused_recordings()?;
        let generation = hex::encode(Sha256::digest(
            serde_json::to_vec(&recordings).map_err(|e| e.to_string())?,
        ));
        let review = RecordingCleanupReview {
            id: uuid::Uuid::new_v4().to_string(),
            generation,
            bytes: recordings.iter().map(|source| source.bytes).sum(),
            recordings,
        };
        let mut reviews = self.video.recording_reviews.lock().unwrap();
        reviews.clear();
        reviews.insert(
            review.id.clone(),
            (std::time::Instant::now(), review.clone()),
        );
        Ok(review)
    }
    // Discard the selected recording review state
    pub fn discard_recording_review(&self, id: &str) {
        self.video.recording_reviews.lock().unwrap().remove(id);
    }
    // Commit recording cleanup
    pub fn commit_recording_cleanup(&self, id: &str) -> Result<u64, String> {
        let _writer = self.video.writer.lock().unwrap();
        let _media = self.profiles.media_gate.exclusive()?;
        let (created, review) = self
            .video
            .recording_reviews
            .lock()
            .unwrap()
            .remove(id)
            .ok_or("Recording cleanup review expired")?;
        if created.elapsed() > std::time::Duration::from_secs(300) {
            return Err("Recording cleanup review expired".into());
        }
        let current = self.unused_recordings()?;
        if hex::encode(Sha256::digest(
            serde_json::to_vec(&current).map_err(|e| e.to_string())?,
        )) != review.generation
        {
            return Err("Recording references changed; review cleanup again".into());
        }
        if review.recordings.is_empty() {
            return Ok(0);
        }
        let directory = self.recording_directory()?;
        for recording in &review.recordings {
            let mut source = std::fs::File::open(self.recording_path(&recording.id)?)
                .map_err(|e| e.to_string())?;
            let before = source.metadata().map_err(|e| e.to_string())?;
            let mut hash = Sha256::new();
            let mut buffer = vec![0; 1024 * 1024];
            loop {
                let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
            }
            if hex::encode(hash.finalize()) != recording.sha256
                || before.modified().ok()
                    != source
                        .metadata()
                        .map_err(|e| e.to_string())?
                        .modified()
                        .ok()
            {
                return Err(
                    "Recording content changed; cleanup stopped without deleting sources".into(),
                );
            }
        }
        let quarantine = directory.join(format!(".cleanup-{}", review.id));
        std::fs::create_dir(&quarantine).map_err(|e| e.to_string())?;
        let ids: Vec<_> = review
            .recordings
            .iter()
            .map(|source| source.id.clone())
            .collect();
        let mut journal =
            std::fs::File::create(quarantine.join("review.json")).map_err(|e| e.to_string())?;
        journal
            .write_all(&serde_json::to_vec(&ids).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        journal.sync_all().map_err(|e| e.to_string())?;
        let transaction = (|| -> Result<(), String> {
            for recording in &review.recordings {
                let id = &recording.id;
                for extension in [recording.source_format.as_str(), "json"] {
                    let name = format!("{id}.{extension}");
                    std::fs::rename(directory.join(&name), quarantine.join(&name))
                        .map_err(|e| e.to_string())?;
                }
            }
            let mut committed =
                std::fs::File::create(quarantine.join("COMMITTED")).map_err(|e| e.to_string())?;
            committed.write_all(b"1").map_err(|e| e.to_string())?;
            committed.sync_all().map_err(|e| e.to_string())?;
            Ok(())
        })();
        recover(&directory)?;
        transaction?;
        Ok(review.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify that unfinished recording cleanup restores sources and committed cleanup finishes
    fn unfinished_recording_cleanup_restores_sources_and_committed_cleanup_finishes() {
        for format in ["mp4", "gif"] {
            let root = tempfile::tempdir().unwrap();
            let id = uuid::Uuid::new_v4().to_string();
            let journal = root
                .path()
                .join(format!(".cleanup-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&journal).unwrap();
            std::fs::write(
                journal.join("review.json"),
                serde_json::to_vec(&[&id]).unwrap(),
            )
            .unwrap();
            std::fs::write(
                journal.join(format!("{id}.{format}")),
                b"retained recording",
            )
            .unwrap();
            std::fs::write(root.path().join(format!("{id}.json")), b"metadata").unwrap();
            recover(root.path()).unwrap();
            assert_eq!(
                std::fs::read(root.path().join(format!("{id}.{format}"))).unwrap(),
                b"retained recording"
            );
            std::fs::create_dir(&journal).unwrap();
            std::fs::write(
                journal.join("review.json"),
                serde_json::to_vec(&[&id]).unwrap(),
            )
            .unwrap();
            std::fs::rename(
                root.path().join(format!("{id}.{format}")),
                journal.join(format!("{id}.{format}")),
            )
            .unwrap();
            std::fs::rename(
                root.path().join(format!("{id}.json")),
                journal.join(format!("{id}.json")),
            )
            .unwrap();
            std::fs::write(journal.join("COMMITTED"), b"1").unwrap();
            recover(root.path()).unwrap();
            assert!(!journal.exists());
            assert!(!root.path().join(format!("{id}.{format}")).exists());
        }
    }
    #[test]
    // Verify that unknown cleanup files are preserved
    fn unknown_cleanup_files_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join(".cleanup-unknown");
        std::fs::create_dir(&journal).unwrap();
        std::fs::write(journal.join("review.json"), b"[]").unwrap();
        std::fs::write(journal.join("unrecognized.mp4"), b"preserve me").unwrap();
        assert!(recover(root.path()).is_err());
        assert_eq!(
            std::fs::read(journal.join("unrecognized.mp4")).unwrap(),
            b"preserve me"
        );
    }
}
