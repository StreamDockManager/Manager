//! WKWebView helpers are launchd services, not children of the application PID
//!
//! Use the application's own launch domain, never a global WebKit name search or
//! responsibility/coalition match: development apps can share those with an IDE
use crate::process_resources::Snapshot;
use std::{collections::BTreeSet, time::Instant};

#[derive(Default)]
pub(super) struct Discovery {
    pub pids: BTreeSet<u32>,
    pub identities: std::collections::HashMap<u32, (u64, u64)>,
    pub complete: bool,
    pub note: &'static str,
}

/// One launch domain per Manager resources sampler, shared by all reported scopes.
#[derive(Default)]
pub(super) struct Cache {
    #[cfg(target_os = "macos")]
    entry: Option<macos::Entry>,
}

impl Cache {
    pub(super) fn invalidate(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.entry = None;
        }
    }

    pub(super) fn discover(&mut self, root: u32, system: &Snapshot, now: Instant) -> Discovery {
        #[cfg(target_os = "macos")]
        {
            macos::discover(&mut self.entry, root, system, now)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (root, system, now);
            Discovery {
                complete: true,
                ..Discovery::default()
            }
        }
    }
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
mod macos {
    use super::*;
    use std::{
        ffi::{c_char, c_int, c_void},
        io::{ErrorKind, Read},
        process::{Command, Stdio},
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    const MAX_OUTPUT: u64 = 2 * 1024 * 1024;
    // Discovery runs only on a sample. An unannounced new helper is picked up
    // at the next requested sample after this provisional freshness interval.
    const FRESHNESS: Duration = Duration::from_secs(5);
    const TIMEOUT: Duration = Duration::from_millis(250);
    const UNAVAILABLE: &str =
        "WebKit helper ownership is unavailable; these readings may exclude UI processes.";
    const INCOMPLETE: &str =
        "Some WebKit helpers could not be measured; these readings may be incomplete.";
    const INCLUDED: &str =
        "Includes this application's macOS WebKit UI, graphics and networking processes.";
    const CACHED: &str =
        "Uses recently verified WebKit ownership. New helpers may be missing until the next discovery, on the first sample after 5 seconds.";

    type Identity = (u32, u64, u64);

    #[derive(Clone, Copy)]
    struct Helper {
        pid: u32,
        identity: Identity,
        name: &'static str,
    }

    pub(super) struct Entry {
        owner: (u32, Identity),
        discovered: Instant,
        helpers: Vec<Helper>,
    }

    impl Entry {
        fn valid(
            &self,
            now: Instant,
            owner: (u32, Identity),
            mut validate: impl FnMut(Helper) -> bool,
        ) -> bool {
            self.owner == owner
                && now.saturating_duration_since(self.discovered) < FRESHNESS
                && self.helpers.iter().copied().all(&mut validate)
        }
    }

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
    fn identity(pid: u32) -> Option<Identity> {
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
        (count == size as c_int
            && info.fields[3] == pid
            && info.start_seconds > 0
            && info.start_micros < 1_000_000)
            .then_some((info.fields[5], info.start_seconds, info.start_micros))
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
        let mut stdout = child.0.stdout.take()?;
        let flags = rustix::fs::fcntl_getfl(&stdout).ok()?;
        rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK).ok()?;
        let deadline = Instant::now() + TIMEOUT;
        let mut output = Vec::new();
        let mut buffer = [0u8; 8192];
        let mut eof = false;
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            if !eof {
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(count) => {
                        if (output.len() + count) as u64 > MAX_OUTPUT {
                            return None;
                        }
                        output.extend_from_slice(&buffer[..count]);
                        continue;
                    }
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(_) => return None,
                }
            }
            match child.0.try_wait().ok()? {
                Some(status) if !status.success() => return None,
                Some(_) if eof => return String::from_utf8(output).ok(),
                _ => thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    fn unavailable() -> Discovery {
        Discovery {
            note: UNAVAILABLE,
            ..Discovery::default()
        }
    }

    fn projection(helpers: &[Helper], complete: bool) -> Discovery {
        Discovery {
            pids: helpers.iter().map(|helper| helper.pid).collect(),
            identities: helpers
                .iter()
                .map(|helper| (helper.pid, (helper.identity.1, helper.identity.2)))
                .collect(),
            complete,
            note: if complete { INCLUDED } else { INCOMPLETE },
        }
    }

    // A process born after a snapshot cannot be matched to its counters, even
    // if sysinfo's seconds-resolution creation time happens to agree.
    fn in_snapshot(system: &Snapshot, pid: u32, proof: Identity) -> bool {
        system
            .processes
            .get(&pid)
            .is_some_and(|process| process.created == proof.1)
            && proof.1.saturating_mul(1000).saturating_add(proof.2 / 1000) <= system.timestamp_ms
    }

    // Fail closed when an owner/helper exits, is reused, or cannot be read.
    // Partial and failed discovery never replace an expired proof as a hit.
    pub(super) fn discover(
        cache: &mut Option<Entry>,
        root: u32,
        system: &Snapshot,
        now: Instant,
    ) -> Discovery {
        discover_with(
            cache,
            root,
            now,
            |pid, proof| in_snapshot(system, pid, proof),
            identity,
            executable,
            || {
                // A candidate born after launchctl starts could be a reused PID
                // from a different domain. Defer it until a fresh observation.
                let time = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
                let began = (time.as_secs(), u64::from(time.subsec_micros()));
                Some((began, parse_services(root, &domain(root)?)?))
            },
        )
    }

    type Catalog = ((u64, u64), Vec<(u32, &'static str)>);

    fn discover_with(
        cache: &mut Option<Entry>,
        root: u32,
        now: Instant,
        has_sample: impl Fn(u32, Identity) -> bool,
        identity: impl Fn(u32) -> Option<Identity>,
        executable: impl Fn(u32) -> Option<String>,
        read_catalog: impl FnOnce() -> Option<Catalog>,
    ) -> Discovery {
        let verified_path = |helper: Helper| {
            executable(helper.pid).is_some_and(|path| apple_executable(&path, helper.name))
                && identity(helper.pid) == Some(helper.identity)
        };
        let Some(root_identity) = identity(root).filter(|proof| has_sample(root, *proof)) else {
            *cache = None;
            return unavailable();
        };
        if let Some(entry) = cache.as_ref().filter(|entry| {
            entry.valid(now, (root, root_identity), |helper| {
                has_sample(helper.pid, helper.identity)
                    && identity(helper.pid) == Some(helper.identity)
                    && verified_path(helper)
            }) && identity(root) == Some(root_identity)
        }) {
            let mut result = projection(&entry.helpers, true);
            result.note = CACHED;
            return result;
        }
        *cache = None;
        let Some((began, services)) = read_catalog() else {
            return unavailable();
        };
        let mut helpers = Vec::new();
        let mut complete = true;
        for (pid, name) in services {
            let valid = identity(pid)
                .map(|identity| Helper {
                    pid,
                    identity,
                    name,
                })
                .filter(|helper| {
                    (helper.identity.1, helper.identity.2) < began
                        && same_live_process(
                            root_identity,
                            helper.identity.1,
                            helper.identity,
                            helper.identity,
                        )
                        && has_sample(helper.pid, helper.identity)
                        && verified_path(*helper)
                });
            if let Some(helper) = valid {
                helpers.push(helper);
            } else {
                complete = false;
            }
        }
        if identity(root) != Some(root_identity) {
            return unavailable();
        }
        let result = projection(&helpers, complete);
        if complete {
            *cache = Some(Entry {
                owner: (root, root_identity),
                discovered: now,
                helpers,
            });
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cache_window_checks_each_helper_and_exact_owner_identity() {
            let now = Instant::now();
            let owner = (10, (501, 100, 10));
            let helper = Helper {
                pid: 20,
                identity: (501, 101, 20),
                name: "com.apple.WebKit.GPU",
            };
            let entry = Entry {
                owner,
                discovered: now,
                helpers: vec![helper],
            };
            assert!(
                entry.valid(now + FRESHNESS - Duration::from_millis(1), owner, |seen| {
                    seen.pid == helper.pid
                        && seen.identity == helper.identity
                        && seen.name == helper.name
                })
            );
            assert!(!entry.valid(now + FRESHNESS, owner, |_| panic!(
                "expired proof revalidated"
            )));
            assert!(!entry.valid(now, (10, (501, 100, 11)), |_| panic!(
                "replaced owner reused"
            )));
            assert!(!entry.valid(now, owner, |seen| seen.identity == (501, 101, 21)));
            assert!(!entry.valid(now, owner, |_| false));
        }

        #[test]
        fn a_verified_empty_domain_differs_from_failed_or_partial_discovery() {
            let now = Instant::now();
            let owner = (10, (501, 100, 1));
            let entry = Entry {
                owner,
                discovered: now,
                helpers: vec![],
            };
            assert!(entry.valid(now, owner, |_| panic!("no helpers")));
            assert!(projection(&entry.helpers, true).complete);
            assert!(!projection(&entry.helpers, false).complete);
            assert!(!unavailable().complete);
            assert_ne!(projection(&entry.helpers, false).note, unavailable().note);
        }

        #[test]
        fn cache_reuses_one_catalog_then_refreshes_and_fails_closed_on_reuse() {
            use std::cell::Cell;

            let mut cache = None;
            let now = Instant::now();
            let micros = Cell::new(10);
            let launches = Cell::new(0);
            let name = "com.apple.WebKit.GPU";
            let sample = |cache: &mut Option<Entry>, at, failed: bool| {
                discover_with(
                    cache,
                    10,
                    at,
                    |_, _| true,
                    |pid| match pid {
                        10 => Some((501, 100, 0)),
                        20 => Some((501, 101, micros.get())),
                        _ => None,
                    },
                    |_| {
                        Some(format!("/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/{name}.xpc/Contents/MacOS/{name}"))
                    },
                    || {
                        launches.set(launches.get() + 1);
                        (!failed).then_some(((102, 0), vec![(20, name)]))
                    },
                )
            };
            assert!(sample(&mut cache, now, false).complete);
            let hit = sample(&mut cache, now + Duration::from_secs(1), true);
            assert!(hit.complete);
            assert_eq!(hit.note, CACHED);
            assert_eq!(launches.get(), 1);
            assert!(sample(&mut cache, now + FRESHNESS, false).complete);
            assert_eq!(launches.get(), 2);
            micros.set(11);
            let broken = sample(&mut cache, now + FRESHNESS + Duration::from_secs(1), true);
            assert!(!broken.complete);
            assert!(broken.pids.is_empty());
            assert!(cache.is_none());
            assert_eq!(launches.get(), 3);
        }

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
        assert!(apple_executable(
            &format!(
                "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/{name}.xpc/Contents/MacOS/{name}"
            ),
            name
        ));
        assert!(!apple_executable(&format!("/tmp/{name}"), name));
        assert!(!apple_executable(
            "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU",
            name
        ));
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
        let mut cache = Cache::default();
        let result = cache.discover(root, &snapshot, Instant::now());
        assert!(!result.pids.is_empty(), "{}", result.note);
        assert!(result.note.starts_with("Includes"), "{}", result.note);
        let own = cache.discover(std::process::id(), &snapshot, Instant::now());
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
