//! Read-only, explicitly selected KDE/logind qualification candidate. No input
#[cfg(target_os = "linux")]
// Run the keyboard Linux session probe example entry point
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let (profile, seconds) = match args.as_slice() {
        [profile_flag, profile, seconds_flag, seconds]
            if profile_flag == "--profile" && seconds_flag == "--seconds" =>
        {
            (profile.as_str(), seconds.parse::<u64>().unwrap_or(0))
        }
        _ => ("", 0),
    };
    if let Err(error) =
        stream_dock_manager_runtime::keyboard_macro::platform::run_linux_kde_session_probe(
            profile,
            std::time::Duration::from_secs(seconds),
        )
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "linux"))]
// Run the keyboard Linux session probe example entry point
fn main() {
    eprintln!("This read-only KDE/logind session probe requires Linux. It does not qualify macOS or Windows.");
    std::process::exit(1);
}
