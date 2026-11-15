use super::*;
#[derive(Clone)]
pub(super) struct DownloadRequest {
    spec: stream_dock_manager_downloads::ArtifactSpec,
    repository: String,
    tag: String,
    product: String,
    name: String,
    selection: Option<Arc<CatalogSelection>>,
}

impl DownloadRequest {
    // Validate a Manager catalog asset and construct its installer download request
    pub(super) fn manager(catalog: &ReleaseCatalog, asset: &ReleaseAsset) -> Result<Self, String> {
        if !asset.supported_installer() {
            return Err("Windows installers must be EXE files".into());
        }
        validate_catalog(
            catalog,
            "manager",
            &catalog.repository,
            &catalog.release_tag,
        )?;
        if !catalog.assets.iter().any(|a| {
            a.name == asset.name
                && a.sha256 == asset.sha256
                && a.size == asset.size
                && a.url == asset.url
        }) {
            return Err("Asset differs from the verified catalog".into());
        }
        Ok(Self {
            spec: stream_dock_manager_downloads::ArtifactSpec {
                identity: format!(
                    "manager:{}:{}:{}:{}:{}:{}",
                    catalog.repository,
                    catalog.release_tag,
                    catalog.commit,
                    asset.name,
                    asset.os,
                    asset.arch
                ),
                url: asset.url.clone(),
                sha256: asset.sha256.clone(),
                size: Some(asset.size),
                limit: asset.size,
            },
            repository: catalog.repository.clone(),
            tag: catalog.release_tag.clone(),
            product: "manager".into(),
            name: asset.name.clone(),
            selection: None,
        })
    }
    // Validate plugin compatibility and bind the download to the selected catalog artifact
    pub(super) fn plugin(selection: Arc<CatalogSelection>) -> Result<Self, String> {
        compatible(&selection.package())?;
        let a = &selection.artifact;
        Ok(Self {
            spec: stream_dock_manager_downloads::ArtifactSpec {
                identity: format!(
                    "plugin:{}:{}:{}:{}:{}:{}:{}",
                    selection.repository,
                    selection.channel,
                    selection.plugin.id,
                    selection.plugin.version,
                    a.source_commit,
                    a.release_tag,
                    a.filename
                ),
                url: selection
                    .github_asset_url
                    .clone()
                    .unwrap_or_else(|| a.url.clone()),
                sha256: a.sha256.clone(),
                size: Some(a.size),
                limit: a.size,
            },
            repository: selection.repository.clone(),
            tag: a.release_tag.clone(),
            product: "plugins".into(),
            name: a.filename.clone(),
            selection: Some(selection),
        })
    }
}
impl UpdateService {
    // Download control
    fn download_control(
        &self,
        id: &str,
        cancel: &AtomicBool,
    ) -> stream_dock_manager_downloads::Control {
        if !cancel.load(Ordering::SeqCst) {
            stream_dock_manager_downloads::Control::Continue
        } else if self.user_cancelled.lock().unwrap().contains(id) {
            stream_dock_manager_downloads::Control::Cancel
        } else {
            stream_dock_manager_downloads::Control::Pause
        }
    }
    // Start a cancellable download job after rechecking the selected catalog entry
    pub(super) fn launch_download(
        self: &Arc<Self>,
        request: DownloadRequest,
    ) -> Result<String, String> {
        if let Some(selection) = &request.selection {
            self.selection_current(selection)?;
        }
        let (id, cancel) = self.job(&request.product, "download")?;
        if let Some(selection) = &request.selection {
            self.selections
                .lock()
                .unwrap()
                .insert(id.clone(), selection.clone());
        }
        self.download_requests
            .lock()
            .unwrap()
            .insert(id.clone(), request.clone());
        if let Some((job, _)) = self.jobs.lock().unwrap().get_mut(&id) {
            job.retry_available = Some(true);
            job.asset_name = Some(request.name.clone());
        }
        let service = self.clone();
        let job = id.clone();
        std::thread::spawn(move || {
            // Retain the source until its completed job is published, so a clear
            // can either skip the active transfer or invalidate a completed job
            let target = service
                .root
                .join("downloads")
                .join(&request.product)
                .join(&request.spec.sha256)
                .join(&request.name);
            let lease = service.cache_lease(&target);
            let result = match &lease {
                Ok(_) => service.transfer_download(&job, &request, &cancel),
                Err(error) => Err(error.clone()),
            };
            service.finish(&job, result.map(|p| Some(p.to_string_lossy().into())));
            drop(lease);
        });
        Ok(id)
    }
    // Retry download
    pub fn retry_download(self: &Arc<Self>, id: &str) -> Result<String, String> {
        {
            let jobs = self.jobs.lock().unwrap();
            let (job, _) = jobs
                .get(id)
                .ok_or("Download job no longer retained; choose the artifact again")?;
            if job.state != "failed" || job.operation != "download" {
                return Err("Only failed downloads can be retried".into());
            }
        }
        let request = self
            .download_requests
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or("Download request no longer retained; choose the artifact again")?;
        let store = self.store.lock().unwrap();
        let preferences = &store.preferences;
        let expected = if request.product == "manager" {
            &preferences.manager_repository
        } else {
            &preferences.plugins_repository
        };
        if request.repository != *expected {
            return Err(
                "Download source changed; review the current catalog before downloading".into(),
            );
        }
        if let Some(selection) = &request.selection {
            if super::plugin_catalog::effective_catalog_url(preferences).ok()
                != selection.catalog_url
            {
                return Err("Plugin catalog source changed; review again".into());
            }
        }
        drop(store);
        self.launch_download(request)
    }
    // Transfer download using the current download jobs state
    pub(super) fn transfer_download(
        &self,
        id: &str,
        request: &DownloadRequest,
        cancel: &AtomicBool,
    ) -> Result<PathBuf, String> {
        let control = || self.download_control(id, cancel);
        let result = (|| -> Result<PathBuf, String> {
            let mut resolved = request.clone();
            if let Some(selection) = &request.selection {
                self.selection_current(selection)?;
                let repository_source = format!(
                    "https://api.github.com/repos/{}/releases",
                    selection.repository
                );
                if selection.catalog_url.as_deref() == Some(repository_source.as_str())
                    && selection.github_asset_url.is_none()
                {
                    let asset = self.plugin_release_asset(
                        &selection.repository,
                        &selection.artifact.release_tag,
                        &selection.artifact.filename,
                        selection.artifact.size,
                        cancel,
                    )?;
                    resolved.spec.url = asset.clone();
                    let mut selected = (**selection).clone();
                    selected.github_asset_url = Some(asset);
                    resolved.selection = Some(Arc::new(selected));
                    self.download_requests
                        .lock()
                        .unwrap()
                        .insert(id.into(), resolved.clone());
                    self.selections
                        .lock()
                        .unwrap()
                        .insert(id.into(), resolved.selection.as_ref().unwrap().clone());
                }
            }
            let request = &resolved;
            let mut directory = self.root.join("downloads");
            for component in ["", request.product.as_str(), request.spec.sha256.as_str()] {
                if !component.is_empty() {
                    directory = directory.join(component);
                }
                if directory
                    .symlink_metadata()
                    .is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir())
                {
                    return Err("Download destination is not an owned directory".into());
                }
                std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
            }
            let target = directory.join(&request.name);
            let _lease = self.cache_lease(&target)?;
            if control() != stream_dock_manager_downloads::Control::Continue {
                return Err("Download stopped before publication".into());
            }
            if target
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink() || !m.is_file())
            {
                return Err("Download destination is not a regular file".into());
            }
            if verify_file(
                &target,
                &request.spec.sha256,
                request.spec.size.ok_or("Missing expected artifact size")?,
            )
            .is_ok()
            {
                self.record_cached_download(&target)?;
                return Ok(target);
            }
            let policy = |url: &Url, redirect: bool| {
                if let Some(selection) = &request.selection {
                    self.selection_current(selection)?;
                    if let Some(initial) = &selection.github_asset_url {
                        super::plugin_github::asset_origin(
                            url,
                            &request.repository,
                            initial,
                            redirect,
                        )
                    } else {
                        super::plugin_catalog::validate_plugin_origin(
                            url.as_str(),
                            &request.repository,
                            &selection.artifact,
                            selection.catalog_url.as_deref(),
                            redirect,
                        )
                    }
                } else {
                    validate_origin(url.as_str(), &request.repository, &request.tag, redirect)
                }
            };
            let token = if request
                .selection
                .as_ref()
                .is_some_and(|s| s.github_asset_url.is_some())
            {
                self.plugin_github_token(&request.repository)?
            } else {
                None
            };
            let headers = |url: &Url| {
                super::plugin_github::asset_headers(url, &request.spec.url, token.as_deref())
            };
            let progress = |completed: u64, total: Option<u64>| {
                if let Some((job, _)) = self.jobs.lock().unwrap().get_mut(id) {
                    job.completed = completed;
                    job.total = total.unwrap_or(0);
                }
                Ok(())
            };
            let artifact = self.downloads.download(
                &request.spec,
                stream_dock_manager_downloads::NetworkPolicy {
                    headers: request
                        .selection
                        .as_ref()
                        .filter(|s| s.github_asset_url.is_some())
                        .map(|_| {
                            &headers as &dyn Fn(&Url) -> Result<reqwest::header::HeaderMap, String>
                        }),
                    validate: &policy,
                    timeout: Duration::from_secs(20),
                },
                &control,
                &progress,
            )?;
            // Final downloads retain their existing explicit user-facing paths;
            // the shared resumable cache owns only bounded retained transfer data
            let mut output =
                tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
            let mut source = std::fs::File::open(artifact.path()).map_err(|e| e.to_string())?;
            let mut buffer = [0; 65536];
            loop {
                if control() != stream_dock_manager_downloads::Control::Continue {
                    return Err("Download stopped before publication".into());
                }
                let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..count])
                    .map_err(|e| e.to_string())?;
            }
            verify_file(output.path(), &request.spec.sha256, artifact.size())?;
            output.as_file().sync_all().map_err(|e| e.to_string())?;
            if control() != stream_dock_manager_downloads::Control::Continue {
                return Err("Download stopped before publication".into());
            }
            output.persist(&target).map_err(|e| e.to_string())?;
            self.record_cached_download(&target)?;
            Ok(target)
        })();
        if control() == stream_dock_manager_downloads::Control::Cancel {
            self.downloads.discard_identity(&request.spec.identity)?;
            return Err("Download cancelled; retained partial discarded".into());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Construct an isolated download jobs fixture
    fn fixture() -> (tempfile::TempDir, Arc<UpdateService>, DownloadRequest) {
        let root = tempfile::tempdir().unwrap();
        let service = UpdateService::open(root.path().join("updates")).unwrap();
        let bytes = b"original verified installer";
        let catalog = ReleaseCatalog {
            schema_version: 1,
            product: "manager".into(),
            repository: "example/manager".into(),
            version: "0.2.0".into(),
            channel: "stable".into(),
            build: "1".into(),
            commit: "a".repeat(40),
            published_at: "2026-10-25T00:00:00Z".into(),
            release_notes: "Fixture".into(),
            release_tag: "v0.2.0".into(),
            assets: vec![ReleaseAsset {
                name: "manager.dmg".into(),
                url: "https://github.com/example/manager/releases/download/v0.2.0/manager.dmg"
                    .into(),
                sha256: hex::encode(Sha256::digest(bytes)),
                size: bytes.len() as u64,
                os: "macos".into(),
                arch: "arm64".into(),
                signing: "unsigned".into(),
            }],
            packages: vec![],
        };
        let request = DownloadRequest::manager(&catalog, &catalog.assets[0]).unwrap();
        service
            .downloads
            .store_reader_from_origin(
                &request.spec,
                &mut std::io::Cursor::new(bytes),
                &|| stream_dock_manager_downloads::Control::Continue,
                &|_, _| Ok(()),
            )
            .unwrap();
        {
            let mut store = service.store.lock().unwrap();
            store.preferences.manager_repository = catalog.repository.clone();
            store.manager.catalog = Some(catalog);
        }
        (root, service, request)
    }
    #[test]
    // Verify that retry keeps the original verified identity when new metadata arrives
    fn retry_keeps_the_original_verified_identity_when_new_metadata_arrives() {
        let (_root, service, request) = fixture();
        let (old, _) = service.job("manager", "download").unwrap();
        service
            .download_requests
            .lock()
            .unwrap()
            .insert(old.clone(), request.clone());
        service.finish(&old, Err("temporary interruption".into()));
        {
            let mut store = service.store.lock().unwrap();
            let catalog = store.manager.catalog.as_mut().unwrap();
            catalog.version = "0.2.1".into();
            catalog.release_tag = "v0.2.1".into();
            catalog.assets[0].sha256 = "b".repeat(64);
        }
        let retry = service.retry_download(&old).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let job = service.jobs.lock().unwrap()[&retry].0.clone();
            if job.state != "running" {
                assert_eq!(job.state, "complete", "{:?}", job.error);
                assert_eq!(job.asset_name.as_deref(), Some("manager.dmg"));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "retained-file retry did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let path = service.completed_download(&retry).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"original verified installer");
        assert_eq!(
            service.download_requests.lock().unwrap()[&retry]
                .spec
                .sha256,
            request.spec.sha256
        );
        assert_eq!(service.status().manager.catalog.unwrap().version, "0.2.1");
    }
    #[test]
    // Verify that publication lease survives transfer until completed job is visible
    fn publication_lease_survives_transfer_until_completed_job_is_visible() {
        let (_root, service, mut request) = fixture();
        // Reuse the verified fixture bytes as a disposable plugin archive; the
        // network cache hit is local and no plugin is executed
        request.product = "plugins".into();
        let target = service
            .root
            .join("downloads/plugins")
            .join(&request.spec.sha256)
            .join(&request.name);
        let publication = service.cache_lease(&target).unwrap();
        let (id, cancel) = service.job("plugins", "download").unwrap();
        let path = service.transfer_download(&id, &request, &cancel).unwrap();
        let other = service.clone();
        assert_eq!(
            std::thread::spawn(move || other.cache_folders(Some(true)).1)
                .join()
                .unwrap(),
            0
        );
        service.finish(&id, Ok(Some(path.to_string_lossy().into_owned())));
        drop(publication);
        assert_eq!(service.cache_folders(Some(true)).1, 1);
        assert_eq!(service.jobs.lock().unwrap()[&id].0.state, "failed");
    }
    #[test]
    // Verify that repository changes and non download jobs cannot retry an old request
    fn repository_changes_and_non_download_jobs_cannot_retry_an_old_request() {
        let (_root, service, request) = fixture();
        let (id, _) = service.job("manager", "download").unwrap();
        service
            .download_requests
            .lock()
            .unwrap()
            .insert(id.clone(), request);
        service.finish(&id, Err("offline".into()));
        service.store.lock().unwrap().preferences.manager_repository = "different/source".into();
        assert!(service
            .retry_download(&id)
            .unwrap_err()
            .contains("source changed"));
        let (check, _) = service.job("manager", "check").unwrap();
        service.finish(&check, Err("offline".into()));
        assert!(service.retry_download(&check).is_err());
    }
    #[test]
    // Verify that shutdown keeps transfer data and explicit cancel discards the owned scope
    fn shutdown_keeps_transfer_data_and_explicit_cancel_discards_the_owned_scope() {
        let (_root, service, request) = fixture();
        let (id, cancel) = service.job("manager", "download").unwrap();
        service.cancel_all();
        assert!(service.transfer_download(&id, &request, &cancel).is_err());
        // A paused job leaves complete or partial transfer checkpoints available
        assert!(service
            .downloads
            .download(
                &request.spec,
                stream_dock_manager_downloads::NetworkPolicy {
                    headers: None,
                    validate: &|_, _| Ok(()),
                    timeout: Duration::from_secs(5)
                },
                &|| stream_dock_manager_downloads::Control::Continue,
                &|_, _| Ok(())
            )
            .is_ok());
        service.cancel(&id).unwrap();
        assert!(service
            .transfer_download(&id, &request, &cancel)
            .unwrap_err()
            .contains("cancelled"));
        let denied = |_: &Url, _: bool| Err("network denied".into());
        assert!(service
            .downloads
            .download(
                &request.spec,
                stream_dock_manager_downloads::NetworkPolicy {
                    headers: None,
                    validate: &denied,
                    timeout: Duration::from_secs(5)
                },
                &|| stream_dock_manager_downloads::Control::Continue,
                &|_, _| Ok(())
            )
            .is_err());
        // The discard also removes idle scope directories rather than leaking
        // metadata-only entries after repeated cancellation
        assert_eq!(
            std::fs::read_dir(service.root.join("partials"))
                .unwrap()
                .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir())
                .count(),
            0
        );
    }
}
