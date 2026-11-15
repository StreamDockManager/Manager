//! Ownership follows a process group / Job Object, never executable names
use std::process::{Child, Command};
pub struct ProcessTree {
    terminated: std::sync::atomic::AtomicBool,
    #[cfg(unix)]
    group: i32,
    #[cfg(windows)]
    job: windows::Win32::Foundation::HANDLE,
}
#[cfg(windows)]
unsafe impl Send for ProcessTree {}
// Configure for the current operation
pub fn configure(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};
        command.creation_flags(CREATE_SUSPENDED.0 | CREATE_NO_WINDOW.0);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = command;
}
impl ProcessTree {
    // Attach for the current operation
    pub fn attach(child: &Child) -> Result<Self, String> {
        #[cfg(unix)]
        {
            Ok(Self {
                group: child.id() as i32,
                terminated: std::sync::atomic::AtomicBool::new(false),
            })
        }
        #[cfg(windows)]
        unsafe {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{Foundation::CloseHandle, System::JobObjects::*};
            let job = CreateJobObjectW(None, None).map_err(|e| e.to_string())?;
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let result = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )
            .and_then(|_| {
                AssignProcessToJobObject(
                    job,
                    windows::Win32::Foundation::HANDLE(child.as_raw_handle()),
                )
            });
            if let Err(error) = result {
                let _ = CloseHandle(job);
                return Err(error.to_string());
            }
            // Resume only after ownership is established, so helpers cannot escape
            // in the interval between OS spawn and assignment to the Job Object.
            let resumed = (|| -> Result<(), String> {
                use windows::Win32::System::{Diagnostics::ToolHelp::*, Threading::*};
                let snapshot =
                    CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).map_err(|e| e.to_string())?;
                let result = (|| {
                    let mut entry = THREADENTRY32 {
                        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
                        ..Default::default()
                    };
                    Thread32First(snapshot, &mut entry).map_err(|e| e.to_string())?;
                    loop {
                        if entry.th32OwnerProcessID == child.id() {
                            let thread =
                                OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
                                    .map_err(|e| e.to_string())?;
                            let resumed = ResumeThread(thread);
                            let _ = CloseHandle(thread);
                            return if resumed == u32::MAX {
                                Err("Could not resume owned plugin process".into())
                            } else {
                                Ok(())
                            };
                        }
                        Thread32Next(snapshot, &mut entry).map_err(|e| e.to_string())?;
                    }
                })();
                let _ = CloseHandle(snapshot);
                result
            })();
            if let Err(error) = resumed {
                let _ = CloseHandle(job);
                return Err(error);
            }
            Ok(Self {
                job,
                terminated: std::sync::atomic::AtomicBool::new(false),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Ok(Self {
                terminated: std::sync::atomic::AtomicBool::new(false),
            })
        }
    }
    // Stop the managed process tree
    pub fn terminate(&self) {
        if self
            .terminated
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-self.group, libc::SIGKILL);
        }
        #[cfg(windows)]
        unsafe {
            let _ = windows::Win32::System::JobObjects::TerminateJobObject(self.job, 1);
        }
    }
}
impl Drop for ProcessTree {
    // Release resources held by this owner
    fn drop(&mut self) {
        self.terminate();
        #[cfg(windows)]
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.job);
        }
    }
}
/// On Unix, observe exit without reaping the root. Its PID/PGID stays reserved
/// until the owned group is killed, preventing termination of a reused PID.
pub fn poll_exit(
    child: &mut Child,
    tree: Option<&ProcessTree>,
) -> Option<std::process::ExitStatus> {
    #[cfg(unix)]
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        if libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        ) != 0
            || info.si_pid() == 0
        {
            return None;
        }
        if let Some(tree) = tree {
            tree.terminate();
        }
    }
    let status = child.try_wait().ok().flatten();
    #[cfg(not(unix))]
    if status.is_some() {
        if let Some(tree) = tree {
            tree.terminate();
        }
    }
    status
}
/// Never wait for exit while a host or global runtime mutex is held
pub fn stop(
    mut child: Child,
    tree: Option<ProcessTree>,
    grace: bool,
) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
    let complete = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = complete.clone();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now()
            + if grace {
                std::time::Duration::from_secs(2)
            } else {
                std::time::Duration::ZERO
            };
        while std::time::Instant::now() < deadline {
            if poll_exit(&mut child, tree.as_ref()).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if let Some(tree) = tree {
            tree.terminate();
        }
        let _ = child.kill();
        let _ = child.wait();
        done.store(true, std::sync::atomic::Ordering::Release);
    });
    complete
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    #[test]
    // Verify owned suspended child has no console and retains output and exit
    fn owned_suspended_child_has_no_console_and_retains_output_and_exit() {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile", "-NonInteractive", "-Command",
            "Add-Type 'using System;using System.Runtime.InteropServices;public class ConsoleProbe{[DllImport(\"kernel32.dll\")]public static extern IntPtr GetConsoleWindow();}';if([ConsoleProbe]::GetConsoleWindow() -ne [IntPtr]::Zero){exit 99};[Console]::Out.Write('owned-output');[Console]::Error.Write('owned-error');exit 17",
        ]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        configure(&mut command);
        let mut child = command.spawn().expect("spawn suspended console probe");
        let tree = match ProcessTree::attach(&child) {
            Ok(tree) => tree,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("attach and resume console probe: {error}");
            }
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                tree.terminate();
                let _ = child.wait();
                panic!("owned console probe did not exit");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(17));
        assert_eq!(output.stdout, b"owned-output");
        assert_eq!(output.stderr, b"owned-error");
    }
}
