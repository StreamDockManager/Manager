//! Explicit, gated native download qualification, separate from ordinary offline tests
use stream_dock_manager_plugin_runtimes::{RuntimeKind, RuntimeService};
// Run this executable entry point
fn main() -> Result<(), String> {
    if std::env::var("STREAM_DOCK_MANAGER_QUALIFY_RUNTIMES").as_deref() != Ok("1") {
        return Err(
            "Set STREAM_DOCK_MANAGER_QUALIFY_RUNTIMES=1 to download official runtimes".into(),
        );
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    let kind = match args.first().map(String::as_str) {
        Some("nodejs") => RuntimeKind::Nodejs,
        _ => return Err("Pass nodejs and a temporary data directory".into()),
    };
    let service =
        RuntimeService::open(args.get(1).ok_or("Pass a qualification directory")?.into())?;
    let status = service.check()?;
    if let Some(error) = status.error {
        eprintln!("Metadata check: {error}");
    }
    let release = status
        .releases
        .into_iter()
        .filter(|r| r.kind == kind)
        .max_by_key(|r| semver::Version::parse(&r.version).unwrap())
        .ok_or("No upstream channel release available")?;
    let job = service.start_install(&release.id)?;
    loop {
        let current = service
            .status()
            .jobs
            .into_iter()
            .find(|j| j.id == job)
            .ok_or("Job disappeared")?;
        match current.state.as_str() {
            "complete" => {
                let selected = service
                    .status()
                    .installations
                    .into_iter()
                    .find(|i| i.id == release.id)
                    .unwrap();
                println!("{}", selected.executable);
                return Ok(());
            }
            "failed" | "cancelled" => {
                return Err(current
                    .error
                    .unwrap_or("Installation did not complete".into()))
            }
            _ => std::thread::sleep(std::time::Duration::from_millis(250)),
        }
    }
}
