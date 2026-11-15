//! Optional, private plugin runtimes. Discovery never downloads executable code
pub mod archives;
pub mod bounded_io;
mod installer;
mod model;
mod providers;
mod service;
pub use model::*;
pub use providers::{eligible, supported};
pub use service::*;

/// Construct a host-owned background tool without creating a Windows console
/// Explicit user application/terminal launches should use their normal command
pub fn background_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(test)]
mod tests;
