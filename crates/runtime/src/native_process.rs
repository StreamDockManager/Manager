//! Subprocesses that implement background native operations, not app launches
pub(crate) use stream_dock_manager_plugin_runtimes::background_command as command;

#[cfg(test)]
mod tests {
    use super::command;
    use std::{io::Write, process::Stdio};

    #[test]
    // Verify that background command keeps pipes and exit status without a Windows console
    fn background_command_keeps_pipes_and_exit_status_without_a_windows_console() {
        #[cfg(windows)]
        let mut command = {
            let mut command = command("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Add-Type 'using System;using System.Runtime.InteropServices;public class ConsoleProbe{[DllImport(\"kernel32.dll\")]public static extern IntPtr GetConsoleWindow();}';[Console]::Write([ConsoleProbe]::GetConsoleWindow().ToInt64().ToString()+':'+[Console]::In.ReadToEnd());[Console]::Error.Write('expected failure');exit 7",
            ]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = command("/bin/sh");
            command.args([
                "-c",
                "printf '0:'; cat; printf 'expected failure' >&2; exit 7",
            ]);
            command
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"native helper input")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(output.stdout, b"0:native helper input");
        assert_eq!(output.stderr, b"expected failure");
    }
}
