//! Read-only AX target lifetime diagnostic. Never captures or sends keyboard input
//! After launch, select the intended ordinary application window within 3 seconds
//! Once pinned, target changes revoke permanently; no automatic retargeting occurs
#[cfg(target_os = "macos")]
// Run the keyboard target probe example entry point
fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let seconds = match args.as_slice() {
        [] => 120,
        [flag, value] if flag == "--seconds" => value.parse::<u64>().unwrap_or(0),
        _ => 0,
    };
    if let Err(error) =
        stream_dock_manager_runtime::keyboard_macro::platform::run_macos_target_probe(
            std::time::Duration::from_secs(seconds),
        )
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "macos"))]
// Run the keyboard target probe example entry point
fn main() {
    eprintln!("This read-only AX target probe requires macOS.");
    std::process::exit(1);
}
