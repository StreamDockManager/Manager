//! R25 release allocator fixture; report bytes and transport behavior are unchanged
use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static REQUESTS: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
}
struct ProbeAllocator;
#[global_allocator]
static ALLOCATOR: ProbeAllocator = ProbeAllocator;
unsafe impl GlobalAlloc for ProbeAllocator {
    // Allocate memory while recording the requested byte count
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: preserve the caller's allocation contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let _ = REQUESTS.try_with(|requests| {
                if let Some((count, bytes)) = requests.get() {
                    requests.set(Some((count + 1, bytes + layout.size())));
                }
            });
        }
        pointer
    }
    // Release memory through the wrapped allocator
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: preserve the original pointer and layout.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[test]
// Verify shared framing allocation probe preserves exact reports
fn shared_framing_allocation_probe_preserves_exact_reports() {
    for data_bytes in [1025usize, 64 * 1024, 1024 * 1024, 16 * 1024 * 1024] {
        let input: Vec<u8> = (0..data_bytes).map(|i| (i % 251) as u8).collect();
        for repetition in 0..5 {
            let baseline_key = "allocation-fixture".to_string();
            REQUESTS.with(|requests| requests.set(Some((0, 0))));
            let baseline = original_transfer(b"BAT", &input, true, baseline_key).unwrap();
            let (baseline_allocations, baseline_requested_bytes) =
                REQUESTS.with(|requests| requests.take().unwrap());
            let key = "allocation-fixture".to_string();
            REQUESTS.with(|requests| requests.set(Some((0, 0))));
            let transaction = transfer(b"BAT", &input, true, key).unwrap();
            let (allocations, requested_bytes) = REQUESTS.with(|requests| requests.take().unwrap());
            assert_eq!(transaction.reports, baseline.reports);
            assert_eq!(transaction.coalesce_key, baseline.coalesce_key);
            assert!(allocations <= baseline_allocations);
            assert!(requested_bytes <= baseline_requested_bytes);
            let expected_reports = data_bytes.div_ceil(PAYLOAD_SIZE) + 2;
            assert_eq!(transaction.reports.capacity(), expected_reports);
            assert_eq!(transaction.reports.len(), expected_reports);
            let mut header = vec![0; PAYLOAD_SIZE + 1];
            header[1..9].copy_from_slice(b"CRT\0\0BAT");
            assert_eq!(transaction.reports[0], header);
            header[6..9].copy_from_slice(b"STP");
            assert_eq!(transaction.reports.last().unwrap(), &header);
            for (chunk, report) in input.chunks(PAYLOAD_SIZE).zip(&transaction.reports[1..]) {
                assert_eq!(report.len(), PAYLOAD_SIZE + 1);
                assert_eq!(report[0], 0);
                assert_eq!(&report[1..1 + chunk.len()], chunk);
                assert!(report[1 + chunk.len()..].iter().all(|byte| *byte == 0));
            }
            let report_bytes: usize = transaction.reports.iter().map(Vec::len).sum();
            let retained_capacity_bytes = transaction.reports.capacity() * size_of::<Vec<u8>>()
                + transaction.reports.iter().map(Vec::capacity).sum::<usize>();
            println!("R25_FRAME_PROBE repetition={repetition} data_bytes={data_bytes} reports={expected_reports} allocations={allocations} baseline_allocations={baseline_allocations} requested_bytes={requested_bytes} baseline_requested_bytes={baseline_requested_bytes} report_bytes={report_bytes} retained_capacity_bytes={retained_capacity_bytes}");
            std::hint::black_box(transaction);
        }
    }
}

// Frozen pre-optimization implementation for byte-for-byte/allocation comparison
fn original_transfer(
    header: &[u8],
    data: &[u8],
    commit: bool,
    key: String,
) -> Result<DeviceTransaction> {
    let mut reports = vec![command(header)?];
    for chunk in data.chunks(PAYLOAD_SIZE) {
        let mut report = vec![0; PAYLOAD_SIZE + 1];
        report[1..1 + chunk.len()].copy_from_slice(chunk);
        reports.push(report);
    }
    if commit {
        reports.push(command(b"STP")?);
    }
    Ok(DeviceTransaction {
        reports,
        coalesce_key: Some(key),
    })
}
