//! Read-only macOS session qualification probe. No key capture/output or OS changes
//! Manually label lock/unlock/sleep/session-switch phases outside its JSON output
//! `observations_healthy` is NOT a screen-unlocked assertion. Revocation is sticky.
#[cfg(target_os = "macos")]
// Run the keyboard session probe example entry point
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let seconds = match args.as_slice() {
        [] => 120,
        [flag, value] if flag == "--seconds" => value.parse::<u64>().unwrap_or(0),
        _ => 0,
    };
    if let Err(error) =
        stream_dock_manager_runtime::keyboard_macro::capture::run_macos_session_probe(
            std::time::Duration::from_secs(seconds),
        )
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "macos"))]
// Run the keyboard session probe example entry point
fn main() {
    eprintln!("This read-only session probe requires macOS.");
    std::process::exit(1);
}
