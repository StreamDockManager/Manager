use clap::{Parser, Subcommand};
use std::{
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};
#[derive(Parser)]
#[command(version, about = "Local Stream Dock device management and diagnostics")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    #[arg(long)]
    config: Option<PathBuf>,
}
#[derive(Subcommand)]
enum Commands {
    /// Inspect, back up, restore, or compact local application data while Manager is quit
    Data {
        #[command(subcommand)]
        operation: DataCommand,
    },
    /// Run the same bounded benchmark service used by Diagnostics. Quit the desktop first
    Benchmark {
        /// JSON BenchmarkConfig exported/documented by the built-in benchmark UI
        #[arg(long)]
        settings: PathBuf,
        /// New JSON report path; existing files are never overwritten
        #[arg(long)]
        output: PathBuf,
        /// Explicitly create a missing simulator in this configuration for this run, then remove it
        #[arg(long)]
        create_simulator: bool,
        /// Optional deterministic cancellation point for automation, in seconds
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..=1800))]
        cancel_after_seconds: Option<u16>,
    },
    Models,
    Devices,
    Diagnostics,
    Listen {
        #[arg(long)]
        device: Option<String>,
    },
    Brightness {
        #[arg(long)]
        device: String,
        value: u8,
    },
    /// Upload PNG/JPEG or loop GIF/MP4 until Enter is pressed
    Image {
        #[arg(long)]
        device: String,
        /// Omit to upload the whole background display
        #[arg(long)]
        control: Option<String>,
        /// Encoding for device image frames, independent of the source media format
        #[arg(long, default_value="png", value_parser=["png","jpeg"])]
        format: String,
        path: PathBuf,
    },
    Lighting {
        #[arg(long)]
        device: String,
        #[arg(long, default_value_t = 65)]
        brightness: u8,
        /// One RGB hex color per descriptor channel, in descriptor order
        colors: Vec<String>,
    },
    Verify {
        #[arg(long)]
        model: String,
        #[arg(long)]
        device: String,
        #[arg(long)]
        firmware: String,
        #[arg(long)]
        driver_revision: String,
        #[arg(long)]
        output: PathBuf,
    },
    Export {
        path: PathBuf,
    },
    Import {
        path: PathBuf,
    },
    /// Install individual local plugin ZIPs while Manager is quit
    InstallPlugins {
        #[arg(required = true, num_args = 1..)]
        paths: Vec<PathBuf>,
    },
}
#[derive(Subcommand)]
enum DataCommand {
    Status,
    Backup {
        /// Include plugin settings in a password-encrypted transfer. Read the password from stdin.
        #[arg(long, requires = "path")]
        password_stdin: bool,
        path: Option<PathBuf>,
    },
    /// Replace managed settings from a validated SQLite snapshot. External media and credentials are not restored
    Restore {
        /// Restore a password-encrypted transfer using one password line from stdin.
        #[arg(long)]
        password_stdin: bool,
        path: PathBuf,
    },
    Compact,
}
// Run this executable entry point
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
// Print the command result in the requested format
fn print(value: impl serde::Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?
    );
    Ok(())
}
// Check whether this CLI command needs device discovery
fn requires_discovery(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Devices
            | Commands::Brightness { .. }
            | Commands::Image { .. }
            | Commands::Lighting { .. }
            | Commands::Verify { .. }
    )
}

// Resolve config
fn resolve_config(
    explicit: Option<PathBuf>,
    default: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<PathBuf, String> {
    explicit.map_or_else(default, Ok)
}

// Run for the current operation
fn run() -> Result<(), String> {
    let cli = Cli::parse();
    if matches!(cli.command, Commands::Models) {
        return print(
            serde_json::json!({"implemented":hardware_registry::modules().iter().filter(|m| m.manifest().variants.iter().any(|v| v.available)).map(|m| {
                let mut manifest = m.manifest().clone(); manifest.variants.retain(|v| v.available); manifest
            }).collect::<Vec<_>>(),"unavailable":hardware_registry::manifests().into_iter().flat_map(|m| m.variants.into_iter().filter(|v| !v.available).map(move |v| serde_json::json!({"model_id":m.model_id,"variant":v}))).collect::<Vec<_>>(),"catalogue":hardware_registry::catalogue()}),
        );
    }
    if matches!(cli.command, Commands::Diagnostics) {
        return print(stream_dock_manager_runtime::diagnostics::collect());
    }
    let root = resolve_config(
        cli.config,
        stream_dock_manager_runtime::Runtime::default_root,
    )?;
    if let Commands::Data { operation } = &cli.command {
        let encrypted = matches!(
            operation,
            DataCommand::Backup {
                password_stdin: true,
                ..
            } | DataCommand::Restore {
                password_stdin: true,
                ..
            }
        );
        let (operation, path) = match operation {
            DataCommand::Status => ("status", None),
            DataCommand::Backup { path, .. } => ("backup", path.clone()),
            DataCommand::Restore { path, .. } => ("restore", Some(path.clone())),
            DataCommand::Compact => ("compact", None),
        };
        let mut password = zeroize::Zeroizing::new(String::new());
        if encrypted {
            use std::io::{BufRead, IsTerminal, Read};
            if std::io::stdin().is_terminal() {
                return Err("Pipe the export password on stdin from a protected source.".into());
            }
            std::io::stdin()
                .lock()
                .take(1027)
                .read_line(&mut password)
                .map_err(|e| e.to_string())?;
            let len = password.trim_end_matches(['\r', '\n']).len();
            if len == 0 || len > 1024 {
                return Err("Invalid transfer password length".into());
            }
            password.truncate(len);
        }
        return print(
            stream_dock_manager_runtime::application_data::maintain_transfer(
                root,
                operation,
                path,
                encrypted.then_some(password.as_str()),
            )?,
        );
    }
    if let Commands::InstallPlugins { paths } = &cli.command {
        let installed = stream_dock_manager_runtime::plugin_packages::install_local_packages(
            &root,
            paths.clone(),
        )?;
        println!(
            "Installed {} plugin(s) into {}",
            installed.len(),
            root.join("plugins").display()
        );
        println!(
            "Saved settings and existing enabled states are preserved. Start Manager to load the builds; enable new plugins in Plugins."
        );
        return Ok(());
    }
    let runtime = stream_dock_manager_runtime::Runtime::new(root.clone())?;
    let events = runtime.subscribe();
    if requires_discovery(&cli.command) {
        if let Err(error) = runtime.wait_for_discovery(Duration::from_secs(10)) {
            runtime.shutdown();
            return Err(error);
        }
    }
    let result = match cli.command {
        Commands::Data { .. } => unreachable!(),
        Commands::Benchmark {
            settings,
            output,
            create_simulator,
            cancel_after_seconds,
        } => run_benchmark(
            &runtime,
            &settings,
            &output,
            create_simulator,
            cancel_after_seconds,
        ),
        Commands::Devices => print(runtime.snapshot().devices),
        Commands::Listen { device } => {
            for event in events {
                if device
                    .as_ref()
                    .is_none_or(|id| event.device_id.as_ref() == Some(id))
                {
                    print(event)?;
                }
            }
            Ok(())
        }
        Commands::Brightness { device, value } => {
            if !runtime.snapshot().devices.iter().any(|d| d.id == device) {
                return Err("device is absent or cannot be opened".into());
            }
            runtime.brightness(&device, value)?;
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        }
        Commands::Image {
            device,
            control,
            format,
            path,
        } => {
            if runtime.upload_image(&device, control.as_deref(), &path, &format)? {
                println!("Playing media. Press Enter to stop.");
                let mut stop = String::new();
                io::stdin()
                    .read_line(&mut stop)
                    .map_err(|e| e.to_string())?;
            } else {
                std::thread::sleep(Duration::from_millis(200));
            }
            Ok(())
        }
        Commands::Lighting {
            device,
            brightness,
            colors,
        } => {
            let colors = colors
                .into_iter()
                .map(|value| {
                    let value = value.trim_start_matches('#');
                    if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                        return Err("colors must contain six hexadecimal digits".to_string());
                    }
                    Ok([
                        u8::from_str_radix(&value[0..2], 16).unwrap(),
                        u8::from_str_radix(&value[2..4], 16).unwrap(),
                        u8::from_str_radix(&value[4..6], 16).unwrap(),
                    ])
                })
                .collect::<Result<Vec<_>, String>>()?;
            runtime.lighting(&device, brightness, colors)?;
            std::thread::sleep(Duration::from_millis(200));
            Ok(())
        }
        Commands::Export { path } => runtime.export_profiles(&path),
        Commands::Import { path } => {
            runtime.import_profiles(&path)?;
            Ok(())
        }
        Commands::Verify {
            model,
            device,
            firmware,
            driver_revision,
            output,
        } => {
            let snapshot = runtime.snapshot();
            let found = snapshot
                .devices
                .iter()
                .find(|d| d.id == device && d.model_id == model && !d.simulated)
                .ok_or("matching physical device not connected")?;
            let modules = hardware_registry::modules();
            let module = modules
                .iter()
                .find(|m| m.manifest().model_id == model)
                .ok_or("verification plan unavailable")?;
            let plan = module.verification_plan().clone();
            if plan.model_id != model {
                return Err("verification plan unavailable".into());
            }
            println!(
                "Run each procedure in hardware/{model}/README.md. Physical results remain unreviewed until a maintainer reviews the evidence."
            );
            let mut features = std::collections::BTreeMap::new();
            for feature in plan.features {
                print!("{feature} [pass/fail/skipped/unavailable]: ");
                io::stdout().flush().map_err(|e| e.to_string())?;
                let mut response = String::new();
                io::stdin()
                    .read_line(&mut response)
                    .map_err(|e| e.to_string())?;
                let outcome = match response.trim() {
                    "pass" => hardware_core::Outcome::Pass,
                    "fail" => hardware_core::Outcome::Fail,
                    "unavailable" => hardware_core::Outcome::Unavailable,
                    _ => hardware_core::Outcome::Skipped,
                };
                features.insert(feature, outcome);
            }
            print!("Evidence references (comma separated): ");
            io::stdout().flush().map_err(|e| e.to_string())?;
            let mut evidence = String::new();
            io::stdin()
                .read_line(&mut evidence)
                .map_err(|e| e.to_string())?;
            let date = std::process::Command::new(if cfg!(windows) { "cmd" } else { "date" })
                .args(if cfg!(windows) {
                    vec!["/C", "echo", "%DATE%"]
                } else {
                    vec!["-u", "+%Y-%m-%dT%H:%M:%SZ"]
                })
                .output()
                .map_err(|e| e.to_string())?;
            let record = hardware_core::VerificationRecord {
                model_id: model,
                variant_id: found.variant_id.clone(),
                usb_identity: format!(
                    "{:?}",
                    found
                        .manifest
                        .variants
                        .iter()
                        .find(|v| v.id == found.variant_id)
                        .unwrap()
                        .identity
                ),
                firmware,
                driver_revision,
                plan_revision: plan.revision,
                platform: std::env::consts::OS.into(),
                architecture: std::env::consts::ARCH.into(),
                desktop_session: std::env::var("XDG_SESSION_TYPE").ok(),
                date: String::from_utf8_lossy(&date.stdout).trim().into(),
                method: hardware_core::VerificationMethod::Physical,
                reviewed_by: None,
                features,
                evidence: evidence
                    .trim()
                    .split(',')
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().into())
                    .collect(),
            };
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)
                .map_err(|e| e.to_string())?;
            file.write_all(&serde_json::to_vec_pretty(&record).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())
        }
        _ => Ok(()),
    };
    runtime.shutdown();
    result
}

// Run benchmark
fn run_benchmark(
    runtime: &std::sync::Arc<stream_dock_manager_runtime::Runtime>,
    settings: &std::path::Path,
    output: &std::path::Path,
    create_simulator: bool,
    cancel_after_seconds: Option<u16>,
) -> Result<(), String> {
    if output.exists() {
        return Err("Benchmark output already exists; choose a new path.".into());
    }
    let metadata = std::fs::metadata(settings).map_err(|e| e.to_string())?;
    if metadata.len() > 65536 {
        return Err("Benchmark settings exceed 64 KiB.".into());
    }
    let config: stream_dock_manager_runtime::benchmark::BenchmarkConfig =
        serde_json::from_slice(&std::fs::read(settings).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut created = None;
    if create_simulator {
        if !["manager", "simulator"].contains(&config.target.as_str()) {
            return Err("--create-simulator is only valid for manager/simulator targets.".into());
        }
        let id = config
            .device_id
            .as_deref()
            .ok_or("Specify device_id for simulator creation.")?;
        if !runtime.snapshot().devices.iter().any(|d| d.id == id) {
            runtime.simulate_device(true, id)?;
            created = Some(id.to_string());
        }
    }
    if config.target == "hardware" {
        runtime.wait_for_discovery(Duration::from_secs(10))?;
    }
    let result = (|| {
        runtime.benchmark_start(config)?;
        let start = std::time::Instant::now();
        let mut cancelled = false;
        loop {
            let view = runtime.benchmark_status();
            if !view.running {
                let report = view.report.ok_or_else(|| {
                    view.error
                        .unwrap_or_else(|| "Benchmark did not produce a report.".into())
                })?;
                let bytes = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(output)
                    .map_err(|e| e.to_string())?;
                file.write_all(&bytes).map_err(|e| e.to_string())?;
                println!(
                    "Benchmark {}: {}. Host completion is not displayed FPS.",
                    report.status,
                    output.display()
                );
                return if report.status == "completed" {
                    Ok(())
                } else {
                    Err(format!(
                        "Benchmark {}; partial report preserved.",
                        report.status
                    ))
                };
            }
            if !cancelled
                && (cancel_after_seconds.is_some_and(|seconds| {
                    start.elapsed() >= Duration::from_secs(u64::from(seconds))
                }) || start.elapsed() >= Duration::from_secs(1830))
            {
                runtime.benchmark_cancel();
                cancelled = true;
            }
            if start.elapsed() >= Duration::from_secs(1860) {
                return Err("Benchmark did not stop within its bounded shutdown window.".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    if let Some(id) = created {
        runtime.remove_simulator(&id)?;
    }
    result
}

#[cfg(test)]
mod benchmark_cli_tests {
    use super::*;

    #[test]
    // Verify benchmark requires explicit input and output
    fn benchmark_requires_explicit_input_and_output() {
        assert!(Cli::try_parse_from(["stream-dock-manager-cli", "benchmark"]).is_err());
        let parsed = Cli::try_parse_from([
            "stream-dock-manager-cli",
            "--config",
            "isolated-data",
            "benchmark",
            "--settings",
            "settings.json",
            "--output",
            "report.json",
            "--create-simulator",
            "--cancel-after-seconds",
            "1",
        ])
        .unwrap();
        assert!(matches!(
            parsed.command,
            Commands::Benchmark {
                create_simulator: true,
                cancel_after_seconds: Some(1),
                ..
            }
        ));
        assert_eq!(parsed.config, Some(PathBuf::from("isolated-data")));
    }

    #[test]
    // Verify benchmark cancellation is bounded
    fn benchmark_cancellation_is_bounded() {
        for seconds in ["0", "1801", "-1", "unlimited"] {
            assert!(Cli::try_parse_from([
                "stream-dock-manager-cli",
                "benchmark",
                "--settings",
                "settings.json",
                "--output",
                "report.json",
                "--cancel-after-seconds",
                seconds,
            ])
            .is_err());
        }
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;
    #[test]
    // Verify discovery is only required by device operations
    fn discovery_is_only_required_by_device_operations() {
        assert!(requires_discovery(&Commands::Devices));
        assert!(requires_discovery(&Commands::Brightness {
            device: "fixture".into(),
            value: 50
        }));
        for command in [
            Commands::Models,
            Commands::Diagnostics,
            Commands::Listen { device: None },
            Commands::Export {
                path: "fixture".into(),
            },
            Commands::Import {
                path: "fixture".into(),
            },
            Commands::Data {
                operation: DataCommand::Status,
            },
            Commands::InstallPlugins { paths: vec![] },
        ] {
            assert!(!requires_discovery(&command));
        }
    }
    #[test]
    // Verify explicit config does not resolve default home
    fn explicit_config_does_not_resolve_default_home() {
        assert_eq!(
            resolve_config(Some("explicit".into()), || panic!("default must stay lazy")).unwrap(),
            PathBuf::from("explicit")
        );
    }
    #[test]
    // Verify missing config uses default and preserves errors
    fn missing_config_uses_default_and_preserves_errors() {
        assert_eq!(
            resolve_config(None, || Ok("default".into())).unwrap(),
            PathBuf::from("default")
        );
        assert_eq!(
            resolve_config(None, || Err("no home".into())).unwrap_err(),
            "no home"
        );
    }
}
