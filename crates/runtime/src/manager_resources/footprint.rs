//! Obtain macOS per-process physical footprint without a task port or a subprocess

#[cfg(target_os = "macos")]
mod macos {
    use std::{
        ffi::{c_char, c_int, c_void},
        io::{Error, ErrorKind},
        mem::{offset_of, size_of},
    };

    const PROC_PIDTBSDINFO: c_int = 3;
    const RUSAGE_INFO_V2: c_int = 2;

    // Match the installed SDK's sys/proc_info.h layout without exposing unrelated process data
    #[repr(C)]
    #[derive(Default)]
    struct BsdInfo {
        before_pid: [u32; 3],
        pid: u32,
        after_pid: [u32; 8],
        comm: [c_char; 16],
        name: [c_char; 32],
        counts: [u32; 5],
        nice: i32,
        start_seconds: u64,
        start_micros: u64,
    }

    // Match the complete sys/resource.h rusage_info_v2 output buffer
    #[repr(C)]
    #[derive(Default)]
    struct UsageV2 {
        uuid: [u8; 16],
        before_footprint: [u64; 7],
        physical_footprint: u64,
        process_start_abstime: u64,
        process_exit_abstime: u64,
        after_exit: [u64; 8],
    }

    const _: () = {
        assert!(size_of::<BsdInfo>() == 136);
        assert!(offset_of!(BsdInfo, start_seconds) == 120);
        assert!(size_of::<UsageV2>() == 160);
        assert!(offset_of!(UsageV2, physical_footprint) == 72);
    };

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            size: c_int,
        ) -> c_int;
        fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut *mut c_void) -> c_int;
    }

    // Translate native failures without returning machine-specific paths or details
    fn native_error(error: Error) -> &'static str {
        match (error.kind(), error.raw_os_error()) {
            (ErrorKind::PermissionDenied, _) => "permission_denied",
            (ErrorKind::NotFound, _) | (_, Some(3)) => "process_exited",
            (ErrorKind::Unsupported, _) | (_, Some(45 | 78)) => "unsupported",
            _ => "sample_unavailable",
        }
    }

    // Read microsecond-resolution process identity and reject short buffer writes
    fn identity(pid: c_int) -> Result<(u64, u64), &'static str> {
        let mut info = BsdInfo::default();
        let size = size_of::<BsdInfo>() as c_int;
        // SAFETY: the flavor writes at most size bytes to a suitably aligned live buffer
        let count = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTBSDINFO,
                0,
                (&mut info as *mut BsdInfo).cast(),
                size,
            )
        };
        if count <= 0 {
            return Err(native_error(Error::last_os_error()));
        }
        if count != size {
            return Err("partial_read");
        }
        if info.pid != pid as u32 || info.start_seconds == 0 || info.start_micros >= 1_000_000 {
            return Err("identity_mismatch");
        }
        Ok((info.start_seconds, info.start_micros))
    }

    // Read the kernel's charged physical memory in bytes for the selected PID
    fn usage(pid: c_int) -> Result<u64, &'static str> {
        let mut info = UsageV2::default();
        // SAFETY: libproc casts its opaque pointer to the full aligned buffer for this flavor
        let status =
            unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V2, (&mut info as *mut UsageV2).cast()) };
        if status != 0 {
            return Err(native_error(Error::last_os_error()));
        }
        if info.process_exit_abstime != 0 {
            return Err("process_exited");
        }
        if info.process_start_abstime == 0 || info.physical_footprint == 0 {
            return Err("partial_read");
        }
        Ok(info.physical_footprint)
    }

    // Match the snapshot identity before querying, then guard against exit or reuse mid-sample
    fn checked(
        created: u64,
        expected: Option<(u64, u64)>,
        mut identity: impl FnMut() -> Result<(u64, u64), &'static str>,
        usage: impl FnOnce() -> Result<u64, &'static str>,
    ) -> Result<u64, &'static str> {
        let before = identity()?;
        if created == 0 || before.0 != created || expected.is_some_and(|value| value != before) {
            return Err("identity_mismatch");
        }
        let bytes = usage()?;
        if identity()? != before {
            return Err("identity_mismatch");
        }
        Ok(bytes)
    }

    // Read a footprint only for the process represented by the snapshot PID and start seconds
    pub(super) fn read(
        pid: u32,
        created: u64,
        expected: Option<(u64, u64)>,
    ) -> Result<u64, &'static str> {
        let pid = c_int::try_from(pid).map_err(|_| "identity_mismatch")?;
        if pid == 0 {
            return Err("identity_mismatch");
        }
        checked(created, expected, || identity(pid), || usage(pid))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        // Reject stale snapshots and same-second PID reuse during the native call
        fn rejects_wrong_creation_and_subsecond_reuse() {
            assert_eq!(
                checked(10, None, || Ok((11, 0)), || panic!("stale PID queried")),
                Err("identity_mismatch")
            );
            assert_eq!(
                checked(
                    10,
                    Some((10, 1)),
                    || Ok((10, 2)),
                    || panic!("reused PID queried")
                ),
                Err("identity_mismatch")
            );
            let mut identities = [Ok((10, 1)), Ok((10, 2))].into_iter();
            assert_eq!(
                checked(10, None, || identities.next().unwrap(), || Ok(1024)),
                Err("identity_mismatch")
            );
        }

        #[test]
        // Distinguish exit and read failure from an observed footprint
        fn preserves_unavailable_reasons() {
            let mut identities = [Ok((10, 1)), Err("process_exited")].into_iter();
            assert_eq!(
                checked(10, None, || identities.next().unwrap(), || Ok(1024)),
                Err("process_exited")
            );
            assert_eq!(
                checked(10, None, || Ok((10, 1)), || Err("permission_denied")),
                Err("permission_denied")
            );
            assert_eq!(checked(10, None, || Ok((10, 1)), || Ok(1024)), Ok(1024));
        }

        #[test]
        // Verify the native ABI against this disposable test process only
        fn reads_own_physical_footprint() {
            let pid = std::process::id();
            let (created, _) = identity(pid.try_into().unwrap()).unwrap();
            assert!(super::super::read(pid, created).unwrap() > 0);
            assert_eq!(
                super::super::read(pid, created + 1),
                Err("identity_mismatch")
            );
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
// Read a process represented only by the whole-second snapshot identity
fn read(pid: u32, created: u64) -> Result<u64, &'static str> {
    read_with_identity(pid, created, None)
}

// Match the native subsecond identity when the attribution probe already captured it
pub(super) fn read_with_identity(
    pid: u32,
    created: u64,
    expected: Option<(u64, u64)>,
) -> Result<u64, &'static str> {
    #[cfg(target_os = "macos")]
    {
        macos::read(pid, created, expected)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, created, expected);
        Err("unsupported")
    }
}
