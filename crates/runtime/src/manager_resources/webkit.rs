//! WKWebView helpers are launchd services, not children of the application PID
//!
//! Use the application's own launch domain, never a global WebKit name search or
//! responsibility/coalition match: development apps can share those with an IDE
use crate::process_resources::Snapshot;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct Discovery {
    pub pids: BTreeSet<u32>,
    pub note: &'static str,
}

#[cfg(not(target_os = "macos"))]
// Discover available webkit candidates
pub(super) fn discover(_: u32, _: &Snapshot) -> Discovery {
    Discovery::default()
}

#[cfg(any(target_os = "macos", test))]
const SERVICES: &[&str] = &[
    "com.apple.WebKit.WebContent",
    "com.apple.WebKit.WebContent.Development",
    "com.apple.WebKit.WebContent.EnhancedSecurity",
    "com.apple.WebKit.Networking",
    "com.apple.WebKit.Networking.Development",
    "com.apple.WebKit.GPU",
    "com.apple.WebKit.GPU.Development",
];

#[cfg(any(target_os = "macos", test))]
// Recognize an allowlisted WebKit service name with an optional valid UUID suffix
fn service_executable(service: &str) -> Option<&'static str> {
    SERVICES.iter().copied().find(|name| {
        service == *name
            || service
                .strip_prefix(name)
                .and_then(|suffix| suffix.strip_prefix('.'))
                .is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
    })
}

#[cfg(any(target_os = "macos", test))]
// Parse services
fn parse_services(root: u32, output: &str) -> Option<Vec<(u32, &'static str)>> {
    let mut lines = output.lines();
    if lines.next()?.trim() != format!("pid/{root} = {{") {
        return None;
    }
    // Only the exact services block contains owned services. The domain also
    // lists inherited service stubs/endpoints belonging to other applications
    let mut in_services = false;
    let mut services = Vec::new();
    for line in lines {
        if !in_services {
            in_services = line == "\tservices = {";
            continue;
        }
        if line == "\t}" {
            return Some(services);
        }
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 3 {
            return None;
        }
        let pid = fields[0].parse::<u32>().ok()?;
        if pid > 0 {
            if let Some(executable) = service_executable(fields[2]) {
                services.push((pid, executable));
                if services.len() > 256 {
                    return None;
                }
            }
        }
    }
    None
}

#[cfg(any(target_os = "macos", test))]
// Report the apple executable condition for webkit
fn apple_executable(path: &str, name: &str) -> bool {
    [
        "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/",
        "/System/Volumes/Preboot/Cryptexes/OS/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/",
    ]
    .iter()
    .any(|root| {
        path.strip_prefix(root)
            .is_some_and(|tail| tail == format!("{name}.xpc/Contents/MacOS/{name}"))
    })
}

#[cfg(any(target_os = "macos", test))]
// Report the same live process condition for webkit
fn same_live_process(
    root: (u32, u64, u64),
    snapshot_start: u64,
    before: (u32, u64, u64),
    after: (u32, u64, u64),
) -> bool {
    before == after
        && before.0 == root.0
        && (before.1, before.2) >= (root.1, root.2)
        && before.1 == snapshot_start
}

#[cfg(target_os = "macos")]
// Discover available webkit candidates
pub(super) fn discover(root: u32, system: &Snapshot) -> Discovery {
    macos::discover(root, system)
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::{
        ffi::{c_char, c_int, c_void},
        io::Read,
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    const MAX_OUTPUT: u64 = 2 * 1024 * 1024;
    const TIMEOUT: Duration = Duration::from_millis(250);
    const UNAVAILABLE: &str =
        "WebKit helper ownership is unavailable; these readings may exclude UI processes.";
    const INCOMPLETE: &str =
        "Some WebKit helpers could not be measured; these readings may be incomplete.";
    const INCLUDED: &str =
        "Includes this application's macOS WebKit UI, graphics and networking processes.";

    // Public libproc.h / sys/proc_info.h ABI (PROC_PIDTBSDINFO = 3)
    #[repr(C)]
    #[derive(Default, PartialEq, Eq)]
    struct BsdInfo {
        fields: [u32; 12],
        comm: [c_char; 16],
        name: [c_char; 32],
        counts: [u32; 5],
        nice: i32,
        start_seconds: u64,
        start_micros: u64,
    }
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            size: c_int,
        ) -> c_int;
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, size: u32) -> c_int;
    }

    // Read parent and creation-time evidence for a macOS process identity
    fn identity(pid: u32) -> Option<(u32, u64, u64)> {
        let mut info = BsdInfo::default();
        let size = std::mem::size_of::<BsdInfo>();
        let count = unsafe {
            proc_pidinfo(
                pid.try_into().ok()?,
                3,
                0,
                (&mut info as *mut BsdInfo).cast(),
                size as c_int,
            )
        };
        (count == size as c_int && info.fields[3] == pid).then_some((
            info.fields[5],
            info.start_seconds,
            info.start_micros,
        ))
    }

    // Read the executable path reported by the macOS process API
    fn executable(pid: u32) -> Option<String> {
        let mut buffer = [0u8; 4096];
        let count = unsafe {
            proc_pidpath(
                pid.try_into().ok()?,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
            )
        };
        if count <= 0 {
            return None;
        }
        let end = buffer.iter().position(|byte| *byte == 0)?;
        std::str::from_utf8(&buffer[..end]).ok().map(str::to_owned)
    }

    // Inspect the root process’s launchd domain without inferring ownership from names alone
    fn domain(root: u32) -> Option<String> {
        // launchctl's print format is not an API contract. Fail closed when it
        // changes, report the limitation, and never infer ownership by name
        let mut command = Command::new("/bin/launchctl");
        command.args(["print", &format!("pid/{root}")]);
        bounded_output(command)
    }

    // Produce bounded output
    fn bounded_output(mut command: Command) -> Option<String> {
        struct Running(std::process::Child);
        impl Drop for Running {
            // Release this webkit owner when it leaves scope
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = Running(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?,
        );
        let stdout = child.0.stdout.take()?;
        let reader = thread::Builder::new()
            .name("webkit-process-scope".into())
            .spawn(move || {
                let mut output = Vec::new();
                stdout.take(MAX_OUTPUT + 1).read_to_end(&mut output).ok()?;
                (output.len() as u64 <= MAX_OUTPUT).then_some(output)
            })
            .ok()?;
        let deadline = Instant::now() + TIMEOUT;
        let success = loop {
            match child.0.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
                _ => {
                    let _ = child.0.kill();
                    let _ = child.0.wait();
                    break false;
                }
            }
        };
        let output = reader.join().ok()??;
        success.then(|| String::from_utf8(output).ok()).flatten()
    }

    // Discover available webkit candidates
    pub(super) fn discover(root: u32, system: &Snapshot) -> Discovery {
        let Some(root_identity) = identity(root) else {
            return Discovery {
                note: UNAVAILABLE,
                ..Discovery::default()
            };
        };
        if !system
            .processes
            .get(&root)
            .is_some_and(|process| process.created == root_identity.1)
        {
            return Discovery {
                note: UNAVAILABLE,
                ..Discovery::default()
            };
        }
        let Some(services) = domain(root).and_then(|output| parse_services(root, &output)) else {
            return Discovery {
                note: UNAVAILABLE,
                ..Discovery::default()
            };
        };
        let mut result = Discovery {
            note: INCLUDED,
            ..Discovery::default()
        };
        for (pid, name) in services {
            let valid = (|| {
                let process = system.processes.get(&pid)?;
                let before = identity(pid)?;
                if !apple_executable(&executable(pid)?, name)
                    || !same_live_process(root_identity, process.created, before, identity(pid)?)
                {
                    return None;
                }
                Some(pid)
            })();
            if let Some(pid) = valid {
                result.pids.insert(pid);
            } else {
                result.note = INCOMPLETE;
            }
        }
        if identity(root) != Some(root_identity) {
            return Discovery {
                note: UNAVAILABLE,
                ..Discovery::default()
            };
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        // Verify that command timeout and excess output fail without hanging
        fn command_timeout_and_excess_output_fail_without_hanging() {
            let began = Instant::now();
            let mut slow = Command::new("/bin/sleep");
            slow.arg("5");
            assert!(bounded_output(slow).is_none());
            assert!(began.elapsed() < Duration::from_secs(3));
            assert!(bounded_output(Command::new("/usr/bin/yes")).is_none());
        }

        #[test]
        // Verify that public bsd identity abi matches the running process
        fn public_bsd_identity_abi_matches_the_running_process() {
            assert_eq!(std::mem::size_of::<BsdInfo>(), 136);
            let pid = std::process::id();
            let identity = identity(pid).expect("read own process identity");
            assert!(identity.1 > 0);
            assert!(identity.2 < 1_000_000);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that only running owned webkit services are discovered
    fn only_running_owned_webkit_services_are_discovered() {
        let output = "pid/10 = {\n\tservices = {\n\t\t20 - com.apple.WebKit.WebContent.292371A7-1633-40F6-A01B-7E11CDEE1E51\n\t\t21 - com.apple.WebKit.GPU\n\t\t22 - com.apple.WebKit.Networking\n\t\t0 - com.apple.WebKit.WebContent\n\t\t23 - com.example.other\n\t\t24 - com.apple.WebKit.WebContent.impostor\n\t}\n\tservice stubs = {\n\t\t99 - com.apple.WebKit.WebContent\n\t}\n}\n";
        assert_eq!(
            parse_services(10, output),
            Some(vec![
                (20, SERVICES[0]),
                (21, SERVICES[5]),
                (22, SERVICES[3])
            ])
        );
        assert!(parse_services(11, output).is_none());
    }

    #[test]
    // Verify that malformed truncated and changed domain output fails closed
    fn malformed_truncated_and_changed_domain_output_fails_closed() {
        for output in [
            "pid/10 = {\n\tservices = {\n\t\t20 - com.apple.WebKit.GPU\n",
            "pid/10 = {\n\tservices = {\n\t\tbad - com.apple.WebKit.GPU\n\t}\n",
            "pid/10 = {\n\tservice stubs = {\n\t\t20 - com.apple.WebKit.GPU\n\t}\n",
        ] {
            assert!(parse_services(10, output).is_none());
        }
        assert_eq!(
            parse_services(10, "pid/10 = {\n\tservices = {\n\t}\n}"),
            Some(vec![])
        );
    }

    #[test]
    // Verify that similarly named user executables are not apple helpers
    fn similarly_named_user_executables_are_not_apple_helpers() {
        let name = "com.apple.WebKit.WebContent";
        assert!(apple_executable(&format!("/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/{name}.xpc/Contents/MacOS/{name}"), name));
        assert!(!apple_executable(&format!("/tmp/{name}"), name));
        assert!(!apple_executable("/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU", name));
    }

    #[test]
    // Verify that process reuse older helpers and other users are rejected
    fn process_reuse_older_helpers_and_other_users_are_rejected() {
        let root = (501, 100, 50);
        let helper = (501, 101, 10);
        assert!(same_live_process(root, 101, helper, helper));
        assert!(!same_live_process(root, 100, helper, helper));
        assert!(!same_live_process(root, 101, helper, (501, 101, 11)));
        assert!(!same_live_process(root, 99, (501, 99, 0), (501, 99, 0)));
        assert!(!same_live_process(
            root,
            100,
            (501, 100, 49),
            (501, 100, 49)
        ));
        assert!(!same_live_process(
            root,
            101,
            (502, 101, 10),
            (502, 101, 10)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires STREAM_DOCK_MANAGER_RESOURCE_PROBE_PID for a running native Manager"]
    // Verify that native webkit owner probe
    fn native_webkit_owner_probe() {
        let root: u32 = std::env::var("STREAM_DOCK_MANAGER_RESOURCE_PROBE_PID")
            .unwrap()
            .parse()
            .unwrap();
        let mut system =
            sysinfo::System::new().expect("process statistics available for native probe");
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::All,
            true,
            sysinfo::ProcessRefreshKind::nothing().with_memory(),
        );
        let snapshot = Snapshot::from_system(&system, std::time::Instant::now(), None);
        let result = discover(root, &snapshot);
        assert!(!result.pids.is_empty(), "{}", result.note);
        assert!(result.note.starts_with("Includes"), "{}", result.note);
        let own = discover(std::process::id(), &snapshot);
        assert!(
            own.pids.is_empty(),
            "A separate test process must not inherit Manager's WebKit helpers"
        );
        println!(
            "Manager PID {root}; verified WebKit helpers {:?}; {}",
            result.pids, result.note
        );
        for pid in result.pids {
            let process = system.process(sysinfo::Pid::from_u32(pid)).unwrap();
            println!("helper PID {pid}: {} resident bytes", process.memory());
        }
    }
}
