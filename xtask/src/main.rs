use clap::{Parser, Subcommand};
use std::path::PathBuf;
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Hardware {
        #[command(subcommand)]
        command: Hardware,
    },
    Types {
        #[arg(long)]
        check: bool,
    },
}
#[derive(Subcommand)]
enum Hardware {
    Check {
        #[arg(long)]
        model: Option<String>,
    },
    Generate,
    Affected {
        paths: Vec<String>,
    },
}
// Run this executable entry point
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1)
    }
}
// Run for the current operation
fn run() -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    match Cli::parse().command {
        Command::Types { check } => {
            use ts_rs::TS;
            // Keep optional dictionary entries in the existing v1 frontend contract
            #[allow(deprecated)]
            let config = ts_rs::Config::default().with_v11_hashmap();
            let declarations = vec![
                stream_dock_manager_runtime::application_data::ApplicationData::decl(&config),
                stream_dock_manager_runtime::cache::CacheFolderStatus::decl(&config),
                stream_dock_manager_runtime::cache::CacheStatus::decl(&config),
                stream_dock_manager_runtime::cache::CacheCleanupResult::decl(&config),
                stream_dock_manager_runtime::application_data::PluginDataUsage::decl(&config),
                stream_dock_manager_runtime::application_data::PluginDataReview::decl(&config),
                stream_dock_manager_runtime::application_data::RecoverySnapshot::decl(&config),
                stream_dock_manager_runtime::application_data::ClientPreferences::decl(&config),
                stream_dock_manager_runtime::application_data::AccessibilityPreferences::decl(
                    &config,
                ),
                stream_dock_manager_runtime::application_data::AccessibilityChoice::decl(&config),
                stream_dock_manager_plugin_host::StoredPluginData::decl(&config),
                stream_dock_manager_plugin_host::SettingsPanel::decl(&config),
                stream_dock_manager_plugin_host::SettingsPanelRequest::decl(&config),
                stream_dock_manager_plugin_host::AppearancePreferences::decl(&config),
                stream_dock_manager_plugin_host::UiThemeStyle::decl(&config),
                stream_dock_manager_plugin_host::ProfilePalette::decl(&config),
                stream_dock_manager_plugin_host::ProfilePaletteTarget::decl(&config),
                stream_dock_manager_plugin_host::CustomPalette::decl(&config),
                stream_dock_manager_plugin_host::DeviceThemePreferences::decl(&config),
                stream_dock_manager_plugin_host::PaletteWallpaper::decl(&config),
                stream_dock_manager_runtime::desktop::AutostartStatus::decl(&config),
                stream_dock_manager_runtime::plugin_packages::PluginReloadResult::decl(&config),
                stream_dock_manager_runtime::plugin_packages::PluginSelectionFailure::decl(&config),
                stream_dock_manager_runtime::plugin_packages::PluginSelectionReview::decl(&config),
                stream_dock_manager_runtime::metrics::PerformanceCache::decl(&config),
                stream_dock_manager_runtime::metrics::PerformanceReport::decl(&config),
                stream_dock_manager_runtime::manager_resources::ManagerResourceReport::decl(
                    &config,
                ),
                stream_dock_manager_runtime::manager_resources::ManagerResourceSample::decl(
                    &config,
                ),
                stream_dock_manager_plugin_host::SettingsLocationFix::decl(&config),
                hardware_core::TouchPanelCapabilities::decl(&config),
                hardware_core::BootLogoCapabilities::decl(&config),
                stream_dock_manager_runtime::boot_logo::BootLogoPreview::decl(&config),
                stream_dock_manager_runtime::boot_logo::BootLogoResult::decl(&config),
                hardware_core::DeviceConfigurationField::decl(&config),
                hardware_core::VibrationCapabilities::decl(&config),
                hardware_core::KeyboardOsMode::decl(&config),
                hardware_core::KeyboardSettings::decl(&config),
                hardware_core::KeyboardBackgroundColor::decl(&config),
                hardware_core::KeyboardCapabilities::decl(&config),
                stream_dock_manager_runtime::keyboard::KeyboardApplyStatus::decl(&config),
                stream_dock_manager_runtime::keyboard::KeyboardApplyResult::decl(&config),
                stream_dock_manager_runtime::keyboard_device::KeyboardDeviceSettings::decl(&config),
                stream_dock_manager_runtime::keyboard_preview::KeyboardPreviewResult::decl(&config),
                stream_dock_manager_runtime::firmware::FirmwareStatus::decl(&config),
                serde_json::Value::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeKind::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeRequirement::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeRelease::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeInstallation::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeSelection::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeResolution::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeJob::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeLine::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeStatus::decl(&config),
                stream_dock_manager_plugin_runtimes::RuntimeUsage::decl(&config),
                hardware_core::appearance::IconAsset::decl(&config),
                hardware_core::appearance::Appearance::decl(&config),
                stream_dock_manager_runtime::icons::IconResults::decl(&config),
                stream_dock_manager_runtime::icons::IconCollection::decl(&config),
                stream_dock_manager_runtime::workspace::AppearancePreset::decl(&config),
                stream_dock_manager_runtime::automation::TitleCondition::decl(&config),
                stream_dock_manager_runtime::automation::Schedule::decl(&config),
                stream_dock_manager_runtime::automation::RuleEvaluation::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryRestorePreview::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryBackup::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryBackups::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryBackupProfile::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryBackupProfiles::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryItem::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryReport::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryDiagnostic::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryVersion::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryVersions::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryDocument::decl(&config),
                stream_dock_manager_runtime::configuration_recovery::ConfigurationRecoveryValidation::decl(&config),
                stream_dock_manager_runtime::onboarding::SetupProgress::decl(&config),
                stream_dock_manager_runtime::onboarding::SetupCheck::decl(&config),
                stream_dock_manager_runtime::onboarding::SetupReport::decl(&config),
                stream_dock_manager_runtime::startup_permissions::StartupPermission::decl(&config),
                stream_dock_manager_runtime::startup_permissions::StartupPermissionReport::decl(&config),
                stream_dock_manager_runtime::setup_capabilities::SetupCapabilityMode::decl(&config),
                stream_dock_manager_runtime::setup_capabilities::SetupCapabilityRequest::decl(
                    &config,
                ),
                stream_dock_manager_runtime::setup_capabilities::SetupCapabilityJob::decl(&config),
                stream_dock_manager_runtime::feedback::AudioState::decl(&config),
                stream_dock_manager_plugin_host::preflight::CompatibilityCheck::decl(&config),
                stream_dock_manager_plugin_host::preflight::PluginPreflight::decl(&config),
                stream_dock_manager_runtime::archive_import::ArchiveImportKind::decl(&config),
                stream_dock_manager_runtime::importing::ImportChoice::decl(&config),
                stream_dock_manager_runtime::importing::ImportIssue::decl(&config),
                stream_dock_manager_runtime::importing::ImportRepair::decl(&config),
                stream_dock_manager_runtime::importing::ImportPreview::decl(&config),
                stream_dock_manager_runtime::plugin_profiles::PluginProfileSuggestion::decl(
                    &config,
                ),
                stream_dock_manager_runtime::plugin_profiles::PluginProfileCatalog::decl(&config),
                stream_dock_manager_runtime::plugin_profiles::PluginPageDestination::decl(&config),
                stream_dock_manager_runtime::plugin_profiles::PluginBindingMapping::decl(&config),
                stream_dock_manager_runtime::plugin_profiles::PluginProfilePlacement::decl(&config),
                stream_dock_manager_runtime::plugin_profiles::PluginImportRow::decl(&config),
                stream_dock_manager_runtime::plugin_profiles::PluginConfigurationField::decl(
                    &config,
                ),
                stream_dock_manager_runtime::plugin_profiles::PluginProfilePreview::decl(&config),
                stream_dock_manager_runtime::export_review::ExportReviewRequest::decl(&config),
                stream_dock_manager_runtime::export_review::ExportField::decl(&config),
                stream_dock_manager_runtime::export_review::ExportReview::decl(&config),
                stream_dock_manager_runtime::export_review::ExportFieldEdit::decl(&config),
                stream_dock_manager_runtime::import_recipes::RememberImportRepair::decl(&config),
                stream_dock_manager_runtime::import_recipes::ImportRepairSuggestion::decl(&config),
                stream_dock_manager_runtime::profile_conversion::ConversionRow::decl(&config),
                stream_dock_manager_runtime::profile_conversion::ConversionPreview::decl(&config),
                stream_dock_manager_runtime::media_library::MediaLibraryEntry::decl(&config),
                stream_dock_manager_runtime::media_library::MediaLibraryPage::decl(&config),
                stream_dock_manager_runtime::media_optimization::MediaOptimizationPreview::decl(&config),
                stream_dock_manager_runtime::media_cleanup::MediaCleanupReview::decl(&config),
                stream_dock_manager_runtime::media_cleanup::MediaCleanupResult::decl(&config),
                stream_dock_manager_runtime::media_deletion::MediaDeletionReview::decl(&config),
                stream_dock_manager_runtime::media_deletion::MediaDeletionResult::decl(&config),
                hardware_core::appearance::ToggleIcon::decl(&config),
                hardware_core::plugin_display::PluginAppearance::decl(&config),
                hardware_core::plugin_display::VendorProfileAppearance::decl(&config),
                hardware_core::plugin_display::VendorTitleStyle::decl(&config),
                stream_dock_manager_runtime::vendor_profiles::VendorProfileReview::decl(&config),
                stream_dock_manager_runtime::vendor_profiles::VendorProfileReviews::decl(&config),
                hardware_core::plugin_display::ElementStyle::decl(&config),
                hardware_core::plugin_display::DisplayElement::decl(&config),
                hardware_core::plugin_display::DisplayState::decl(&config),
                hardware_core::composition::DisplaySource::decl(&config),
                hardware_core::composition::DisplayLayer::decl(&config),
                hardware_core::composition::DisplayComposition::decl(&config),
                hardware_core::composition::DisplaySourceDescriptor::decl(&config),
                stream_dock_manager_runtime::preview::BindingPreview::decl(&config),
                hardware_core::HidIdentity::decl(&config),
                hardware_core::HidSelector::decl(&config),
                hardware_core::DeviceCapabilities::decl(&config),
                hardware_core::FirmwareReport::decl(&config),
                hardware_core::FirmwareReportKind::decl(&config),
                hardware_core::FrameOutputCapabilities::decl(&config),
                hardware_core::OutputMode::decl(&config),
                hardware_core::FrameEncoding::decl(&config),
                hardware_core::OutputPolicy::decl(&config),
                hardware_core::OutputStatus::decl(&config),
                hardware_core::OutputOutcome::decl(&config),
                hardware_core::OutputQueueStats::decl(&config),
                hardware_core::Layout::decl(&config),
                hardware_core::Control::decl(&config),
                hardware_core::DirectionalSwitch::decl(&config),
                hardware_core::Variant::decl(&config),
                hardware_core::ModelManifest::decl(&config),
                hardware_core::Input::decl(&config),
                hardware_core::ControlEvent::decl(&config),
                stream_dock_manager_runtime::builtins::EffectiveBinding::decl(&config),
                stream_dock_manager_runtime::builtins::Condition::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroCapabilities::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroCaptureMode::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroCaptureEvent::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroSessionState::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroSession::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::recorder::KeyboardMacroRecordingResult::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::test_session::KeyboardMacroTestState::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::test_session::KeyboardMacroTestSession::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::PhysicalKey::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroEventKind::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroEvent::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroPlatform::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroKeyMode::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroRecording::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroFocusPolicy::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroRepeatMode::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroPlayback::decl(&config),
                stream_dock_manager_runtime::keyboard_macro::KeyboardMacroSummary::decl(&config),
                stream_dock_manager_runtime::builtins::BuiltinAction::decl(&config),
                stream_dock_manager_runtime::builtins::SavedMacro::decl(&config),
                stream_dock_manager_runtime::macro_inputs::MacroInput::decl(&config),
                stream_dock_manager_runtime::macro_inputs::MacroArgument::decl(&config),
                stream_dock_manager_runtime::actions::Action::decl(&config),
                stream_dock_manager_runtime::actions::NavigationTarget::decl(&config),
                stream_dock_manager_runtime::actions::NavigationDirection::decl(&config),
                stream_dock_manager_runtime::media::MediaOptions::decl(&config),
                stream_dock_manager_runtime::video::Recording::decl(&config),
                stream_dock_manager_runtime::video::ClipInfo::decl(&config),
                stream_dock_manager_runtime::video::ClipDraft::decl(&config),
                stream_dock_manager_runtime::video::VideoJob::decl(&config),
                stream_dock_manager_runtime::video::VideoTarget::decl(&config),
                stream_dock_manager_runtime::video::VideoClipEntry::decl(&config),
                stream_dock_manager_runtime::video::VideoClipDetails::decl(&config),
                stream_dock_manager_runtime::video_recordings::RecordingCleanupReview::decl(
                    &config,
                ),
                stream_dock_manager_runtime::preparation::MediaPreparation::decl(&config),
                stream_dock_manager_runtime::workspace::DeviceSession::decl(&config),
                stream_dock_manager_runtime::workspace::DeviceOutput::decl(&config),
                stream_dock_manager_runtime::workspace::DeviceFramePolicy::decl(&config),
                stream_dock_manager_runtime::power_policy::PowerAction::decl(&config),
                stream_dock_manager_runtime::power_policy::PowerPlatformCapabilities::decl(&config),
                stream_dock_manager_runtime::power_policy::DevicePowerCapabilities::decl(&config),
                stream_dock_manager_runtime::power_policy::DevicePowerPolicy::decl(&config),
                stream_dock_manager_runtime::power_policy::DevicePowerStatus::decl(&config),
                stream_dock_manager_runtime::power_policy::DevicePowerSettings::decl(&config),
                stream_dock_manager_runtime::workspace::AppRule::decl(&config),
                stream_dock_manager_runtime::workspace::HistoryItem::decl(&config),
                stream_dock_manager_runtime::history_compare::HistoryFieldChange::decl(&config),
                stream_dock_manager_runtime::history_compare::HistoryControlAddress::decl(&config),
                stream_dock_manager_runtime::history_compare::HistoryChange::decl(&config),
                stream_dock_manager_runtime::history_compare::HistoryComparison::decl(&config),
                stream_dock_manager_runtime::library_organization::LibraryIdentity::decl(&config),
                stream_dock_manager_runtime::library_organization::LibraryMark::decl(&config),
                stream_dock_manager_runtime::library_organization::LibraryOrganization::decl(
                    &config,
                ),
                stream_dock_manager_runtime::history_compare::StaticHistoryPreview::decl(&config),
                stream_dock_manager_runtime::history_compare::HistoryControlComparison::decl(
                    &config,
                ),
                stream_dock_manager_runtime::workspace::HistoryIndex::decl(&config),
                stream_dock_manager_runtime::workspace::Draft::decl(&config),
                stream_dock_manager_runtime::workspace::AssignmentScope::decl(&config),
                stream_dock_manager_runtime::workspace::Edit::decl(&config),
                stream_dock_manager_runtime::automation::AutomationStatus::decl(&config),
                stream_dock_manager_runtime::automation::AutomationConfigurationDraft::decl(&config),
                stream_dock_manager_runtime::metrics::Metric::decl(&config),
                stream_dock_manager_runtime::benchmark::BenchmarkConfig::decl(&config),
                stream_dock_manager_runtime::benchmark::BenchmarkPercentiles::decl(&config),
                stream_dock_manager_runtime::benchmark::BenchmarkSurface::decl(&config),
                stream_dock_manager_runtime::benchmark::BenchmarkRun::decl(&config),
                stream_dock_manager_runtime::benchmark::BenchmarkReport::decl(&config),
                stream_dock_manager_runtime::benchmark::BenchmarkView::decl(&config),
                stream_dock_manager_runtime::benchmark::BenchmarkProgress::decl(&config),
                stream_dock_manager_runtime::benchmark::BenchmarkResult::decl(&config),
                stream_dock_manager_runtime::preview::PreviewDelta::decl(&config),
                stream_dock_manager_plugin_host::MeasurementState::decl(&config),
                stream_dock_manager_plugin_host::MeasurementSource::decl(&config),
                stream_dock_manager_plugin_host::MeasurementCoverage::decl(&config),
                stream_dock_manager_plugin_host::RuntimeState::decl(&config),
                stream_dock_manager_plugin_host::RuntimeReason::decl(&config),
                stream_dock_manager_plugin_host::PluginDemand::decl(&config),
                stream_dock_manager_plugin_host::ResourceSample::decl(&config),
                stream_dock_manager_plugin_host::PluginResources::decl(&config),
                stream_dock_manager_plugin_host::PluginNetwork::decl(&config),
                stream_dock_manager_plugin_host::PluginRuntime::decl(&config),
                stream_dock_manager_plugin_host::HealthCounts::decl(&config),
                stream_dock_manager_plugin_host::HealthEvent::decl(&config),
                stream_dock_manager_plugin_host::PluginRuntimeSnapshot::decl(&config),
                stream_dock_manager_plugin_host::PackageSource::decl(&config),
                stream_dock_manager_plugin_host::manifest::NormalizedManifest::decl(&config),
                stream_dock_manager_plugin_host::PluginHealth::decl(&config),
                stream_dock_manager_runtime::profiles::BackgroundSource::decl(&config),
                stream_dock_manager_runtime::Binding::decl(&config),
                stream_dock_manager_plugin_host::recovery::RecoveryStatus::decl(&config),
                stream_dock_manager_plugin_host::manager_diagnostics::FrontendDiagnostic::decl(
                    &config,
                ),
                stream_dock_manager_plugin_host::manager_diagnostics::FrontendFrame::decl(&config),
                stream_dock_manager_plugin_host::manager_diagnostics::FrontendComponent::decl(
                    &config,
                ),
                stream_dock_manager_plugin_host::manager_diagnostics::FrontendEvent::decl(&config),
                stream_dock_manager_plugin_host::manager_diagnostics::FailureReason::decl(&config),
                stream_dock_manager_plugin_host::recovery::InterruptedPlugin::decl(&config),
                stream_dock_manager_runtime::dial_response::DialCurve::decl(&config),
                stream_dock_manager_runtime::dial_response::DialResponse::decl(&config),
                stream_dock_manager_runtime::artwork::ArtworkSlice::decl(&config),
                stream_dock_manager_runtime::hardware_preview::HardwareDraft::decl(&config),
                stream_dock_manager_runtime::hardware_preview::BackgroundDraft::decl(&config),
                stream_dock_manager_runtime::Page::decl(&config),
                stream_dock_manager_runtime::Profile::decl(&config),
                stream_dock_manager_runtime::parameters::ProfileParameter::decl(&config),
                stream_dock_manager_runtime::parameters::ParameterOverrides::decl(&config),
                stream_dock_manager_runtime::credentials::CredentialState::decl(&config),
                stream_dock_manager_runtime::parameter_credentials::CredentialTarget::decl(&config),
                stream_dock_manager_runtime::parameter_credentials::CredentialWrite::decl(&config),
                stream_dock_manager_plugin_host::ThemedAsset::decl(&config),
                stream_dock_manager_plugin_host::ActionCapability::decl(&config),
                stream_dock_manager_plugin_host::RefreshPolicy::decl(&config),
                stream_dock_manager_plugin_host::Capabilities::decl(&config),
                stream_dock_manager_plugin_host::PluginAction::decl(&config),
                stream_dock_manager_plugin_host::PluginInfo::decl(&config),
                stream_dock_manager_runtime::DeviceInfo::decl(&config),
                stream_dock_manager_runtime::Snapshot::decl(&config),
                stream_dock_manager_runtime::workspace_stream::WorkspaceBatch::decl(&config),
                stream_dock_manager_runtime::actions::ActivationPolicy::decl(&config),
                stream_dock_manager_runtime::actions::RunOutcome::decl(&config),
                stream_dock_manager_runtime::actions::RunId::decl(&config),
                stream_dock_manager_runtime::macro_debug::DebugMode::decl(&config),
                stream_dock_manager_runtime::macro_debug::DebugStatus::decl(&config),
                stream_dock_manager_runtime::macro_debug::DebugCommand::decl(&config),
                stream_dock_manager_runtime::macro_debug::DebugStart::decl(&config),
                stream_dock_manager_runtime::macro_debug::SimulationConfig::decl(&config),
                stream_dock_manager_runtime::macro_debug::DebugView::decl(&config),
                stream_dock_manager_runtime::macro_debug::DebugStepEvent::decl(&config),
                stream_dock_manager_runtime::workspace_stream::WorkspaceChanges::decl(&config),
                stream_dock_manager_runtime::SurfacePreview::decl(&config),
                stream_dock_manager_runtime::RuntimeEvent::decl(&config),
                stream_dock_manager_runtime::diagnostics::Diagnostic::decl(&config),
                stream_dock_manager_runtime::desktop::DesktopSettings::decl(&config),
                stream_dock_manager_runtime::desktop::PluginLinkStatus::decl(&config),
                stream_dock_manager_runtime::desktop::EditorRestoreRoute::decl(&config),
                stream_dock_manager_runtime::desktop::EditorRestoreDescriptor::decl(&config),
                stream_dock_manager_runtime::desktop::EditorRestoreState::decl(&config),
                stream_dock_manager_runtime::updates::UpdatePreferences::decl(&config),
                stream_dock_manager_runtime::updates::ReleaseAsset::decl(&config),
                stream_dock_manager_runtime::updates::PackageCandidate::decl(&config),
                stream_dock_manager_runtime::updates::ReleaseCatalog::decl(&config),
                stream_dock_manager_runtime::updates::ManagerReleaseLink::decl(&config),
                stream_dock_manager_runtime::updates::ProjectLink::decl(&config),
                stream_dock_manager_runtime::updates::PluginCatalog::decl(&config),
                stream_dock_manager_runtime::updates::CatalogPlugin::decl(&config),
                stream_dock_manager_runtime::updates::PluginArtifact::decl(&config),
                stream_dock_manager_runtime::updates::UnifiedPluginArtifact::decl(&config),
                stream_dock_manager_runtime::updates::UnifiedPluginTarget::decl(&config),
                stream_dock_manager_runtime::updates::PluginDependency::decl(&config),
                stream_dock_manager_runtime::updates::PluginDocumentationRequest::decl(&config),
                stream_dock_manager_runtime::updates::VsdCompatibility::decl(&config),
                stream_dock_manager_runtime::updates::QualificationEvidence::decl(&config),
                stream_dock_manager_runtime::updates::CheckState::decl(&config),
                stream_dock_manager_runtime::updates::UpdateJob::decl(&config),
                stream_dock_manager_runtime::updates::PluginUpdate::decl(&config),
                stream_dock_manager_runtime::updates::UpdateStatus::decl(&config),
                stream_dock_manager_runtime::ProfileExportOptions::decl(&config),
                stream_dock_manager_runtime::macro_sharing::MacroImportItem::decl(&config),
                stream_dock_manager_runtime::macro_sharing::MacroImportPreview::decl(&config),
                stream_dock_manager_runtime::plugin_packages::PluginPackageReview::decl(&config),
                stream_dock_manager_plugin_host::backups::PluginBackup::decl(&config),
                stream_dock_manager_plugin_host::backups::PluginBackups::decl(&config),
                stream_dock_manager_plugin_host::backups::InstalledPackageIdentity::decl(&config),
                stream_dock_manager_plugin_host::backups::BackupReview::decl(&config),
                stream_dock_manager_runtime::plugin_rollback::PluginRollbackReview::decl(&config),
                stream_dock_manager_runtime::plugin_policy::PluginUpdatePolicy::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogPreferences::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogPluginPreferences::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogCaptureStatus::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogPluginIdentity::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogEntry::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogNetworkBody::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogNetworkDetail::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogNetworkHeader::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogNetworkHeaders::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogCursor::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogPage::decl(&config),
                stream_dock_manager_plugin_host::log_capture::LogReaderLease::decl(&config),
                stream_dock_manager_runtime::resource_alerts::ResourceAlertPreferences::decl(
                    &config,
                ),
                stream_dock_manager_runtime::resource_alerts::ResourceAlertSettings::decl(&config),
            ];
            let output = "// Generated by cargo run -p xtask -- types. Do not edit.\n".to_string()
                + &declarations
                    .into_iter()
                    .map(|s| {
                        format!(
                            "export {}\n",
                            s.lines().map(str::trim_end).collect::<Vec<_>>().join("\n")
                        )
                    })
                    .collect::<String>();
            if !check {
                std::fs::create_dir_all(root.join("generated")).map_err(|e| e.to_string())?;
            }
            generated(&root, "generated/runtime.ts", output.as_bytes(), check)?;
            let keyboard_keys = serde_json::to_string_pretty(
                stream_dock_manager_runtime::keyboard_macro::PhysicalKey::ALL,
            )
            .map_err(|e| e.to_string())?;
            let keyboard_limits = serde_json::json!({
                "maxEvents": stream_dock_manager_runtime::keyboard_macro::MAX_EVENTS,
                "maxDurationMs": stream_dock_manager_runtime::keyboard_macro::MAX_DURATION_MS,
                "maxSerializedBytes": stream_dock_manager_runtime::keyboard_macro::MAX_SERIALIZED_BYTES,
                "minSpeed": stream_dock_manager_runtime::keyboard_macro::MIN_SPEED,
                "maxSpeed": stream_dock_manager_runtime::keyboard_macro::MAX_SPEED,
                "version": stream_dock_manager_runtime::keyboard_macro::RECORDING_VERSION,
            });
            let keyboard_contract = format!("// Generated by cargo run -p xtask -- types. Do not edit.\nimport type {{ PhysicalKey }} from './runtime';\nexport const PHYSICAL_KEYS: readonly PhysicalKey[] = {keyboard_keys};\nexport const KEYBOARD_MACRO_LIMITS = {} as const;\n", serde_json::to_string_pretty(&keyboard_limits).map_err(|e|e.to_string())?);
            generated(
                &root,
                "generated/keyboard-macro.ts",
                keyboard_contract.as_bytes(),
                check,
            )?;

            let commands = stream_dock_manager_desktop_contract::discover(&root)?;
            stream_dock_manager_desktop_contract::validate_capabilities(&root, &commands)?;
            let desktop = stream_dock_manager_desktop_contract::typescript(&commands, &output)?;
            generated(&root, "generated/commands.ts", desktop.as_bytes(), check)?;
            generated(
                &root,
                "hardware/schema.json",
                &serde_json::to_vec_pretty(&schemars::schema_for!(hardware_core::ModelManifest))
                    .map_err(|e| e.to_string())?,
                check,
            )?;
            generated(
                &root,
                "generated/plugin-runtime.schema.json",
                &serde_json::to_vec_pretty(&schemars::schema_for!(
                    Vec<stream_dock_manager_plugin_host::PluginRuntimeSnapshot>
                ))
                .map_err(|e| e.to_string())?,
                check,
            )?;
            generated(
                &root,
                "generated/plugin-profile.schema.json",
                &serde_json::to_vec_pretty(&schemars::schema_for!(
                    stream_dock_manager_runtime::plugin_profiles::PluginProfileDocument
                ))
                .map_err(|e| e.to_string())?,
                check,
            )?;
            generated(
                &root,
                "generated/plugin-package-review.schema.json",
                &serde_json::to_vec_pretty(&schemars::schema_for!(
                    stream_dock_manager_runtime::plugin_packages::PluginPackageReview
                ))
                .map_err(|e| e.to_string())?,
                check,
            )?;
            generated(
                &root,
                "generated/snapshot.schema.json",
                &serde_json::to_vec_pretty(&schemars::schema_for!(
                    stream_dock_manager_runtime::Snapshot
                ))
                .map_err(|e| e.to_string())?,
                check,
            )?;
            Ok(())
        }
        Command::Hardware { command } => match command {
            Hardware::Check { model } => {
                hardware_registry::validate().map_err(|e| e.to_string())?;
                let modules = hardware_registry::modules();
                if model
                    .as_ref()
                    .is_some_and(|id| !modules.iter().any(|m| m.manifest().model_id == *id))
                {
                    return Err("model has no registered module".into());
                }
                let registered: std::collections::HashSet<_> = modules
                    .iter()
                    .map(|m| m.manifest().model_id.clone())
                    .collect();
                for entry in std::fs::read_dir(root.join("hardware")).map_err(|e| e.to_string())? {
                    let p = entry.map_err(|e| e.to_string())?.path();
                    if p.join("device.toml").exists() {
                        let m = hardware_core::ModelManifest::parse(
                            &std::fs::read_to_string(p.join("device.toml"))
                                .map_err(|e| e.to_string())?,
                        )
                        .map_err(|e| e.to_string())?;
                        if !registered.contains(&m.model_id) {
                            return Err(format!("unregistered device module: {}", m.model_id));
                        }
                    }
                }
                for module in modules {
                    let m = module.manifest();
                    if model.as_ref().is_some_and(|id| id != &m.model_id) {
                        continue;
                    }
                    let folder = root.join("hardware").join(&m.model_id);
                    for required in [
                        "Cargo.toml",
                        "README.md",
                        "src/lib.rs",
                        "tests",
                        "fixtures",
                        "verification/plan.toml",
                        "verification/results",
                    ] {
                        if !folder.join(required).exists() {
                            return Err(format!("{}: missing {required}", m.model_id));
                        }
                    }
                    let plan: hardware_core::VerificationPlan = toml::from_str(
                        &std::fs::read_to_string(folder.join("verification/plan.toml"))
                            .map_err(|e| e.to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                    if plan.model_id != m.model_id
                        || plan.features.len() < 10
                        || plan.revision.is_empty()
                    {
                        return Err("incomplete verification plan".into());
                    }
                    let mut fixture_count = 0;
                    for entry in
                        std::fs::read_dir(folder.join("fixtures")).map_err(|e| e.to_string())?
                    {
                        let path = entry.map_err(|e| e.to_string())?.path();
                        if path.extension().is_some_and(|s| s == "json") {
                            let fixture: serde_json::Value = serde_json::from_slice(
                                &std::fs::read(&path).map_err(|e| e.to_string())?,
                            )
                            .map_err(|e| e.to_string())?;
                            if !["simulated", "replay", "physical"]
                                .contains(&fixture["method"].as_str().unwrap_or(""))
                                || fixture["provenance"].as_str().unwrap_or("").is_empty()
                            {
                                return Err(format!(
                                    "fixture method or provenance missing: {}",
                                    path.display()
                                ));
                            }
                            fixture_count += 1;
                        }
                    }
                    if fixture_count == 0 {
                        return Err("module has no provenance-labelled fixtures".into());
                    }
                    for entry in std::fs::read_dir(folder.join("verification/results"))
                        .map_err(|e| e.to_string())?
                    {
                        let path = entry.map_err(|e| e.to_string())?.path();
                        if path.extension().is_some_and(|s| s == "json") {
                            let record: hardware_core::VerificationRecord = serde_json::from_slice(
                                &std::fs::read(&path).map_err(|e| e.to_string())?,
                            )
                            .map_err(|e| e.to_string())?;
                            if record.model_id != m.model_id
                                || !m.variants.iter().any(|v| v.id == record.variant_id)
                                || record.driver_revision.is_empty()
                                || record.plan_revision.is_empty()
                            {
                                return Err(format!(
                                    "invalid verification record {}",
                                    path.display()
                                ));
                            }
                        }
                    }
                    println!(
                        "{}: descriptor, registration, controls, fixtures, plan OK (physical qualification remains separate)",
                        m.model_id
                    );
                }
                let path = "packaging/linux/60-stream-dock-manager.rules";
                if std::fs::read_to_string(root.join(path)).unwrap_or_default()
                    != hardware_registry::udev_rules()
                {
                    return Err(format!(
                        "stale generated file {path}; run hardware generate"
                    ));
                }
                Ok(())
            }
            Hardware::Generate => {
                let path = root.join("packaging/linux/60-stream-dock-manager.rules");
                std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
                std::fs::write(path, hardware_registry::udev_rules()).map_err(|e| e.to_string())?;
                let manifests = hardware_registry::manifests();
                std::fs::write(
                    root.join("public/hardware.json"),
                    serde_json::to_vec_pretty(&manifests).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                Ok(())
            }
            Hardware::Affected { paths } => {
                let shared = paths
                    .iter()
                    .any(|p| p.starts_with("crates/hardware-") || p.starts_with("Cargo"));
                for m in hardware_registry::modules() {
                    let id = &m.manifest().model_id;
                    if shared
                        || paths
                            .iter()
                            .any(|p| p.starts_with(&format!("hardware/{id}/")))
                    {
                        println!(
                            "{id}: run module suite; historical verification records require revision review"
                        );
                    }
                }
                Ok(())
            }
        },
    }
}

// Generate the requested checked-in contract output
fn generated(
    root: &std::path::Path,
    relative: &str,
    bytes: &[u8],
    check: bool,
) -> Result<(), String> {
    let path = root.join(relative);
    if check {
        if std::fs::read(&path).ok().as_deref() != Some(bytes) {
            return Err(format!(
                "Stale generated file {relative}; run cargo run -p xtask -- types"
            ));
        }
        Ok(())
    } else {
        std::fs::write(path, bytes).map_err(|e| e.to_string())
    }
}
