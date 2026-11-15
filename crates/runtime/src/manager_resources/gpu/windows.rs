//! PDH GPU engine counters, filtered by exact process ID and aggregated per engine
use super::Reading;
use std::collections::{BTreeMap, BTreeSet};
use windows::{
    core::{w, PCWSTR},
    Win32::System::Performance::*,
};
#[derive(Default)]
pub(super) struct Sampler {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    warmed: bool,
}
// The owning Manager sampler serializes every use, including destruction
unsafe impl Send for Sampler {}
impl Drop for Sampler {
    // Release this Windows owner when it leaves scope
    fn drop(&mut self) {
        if !self.query.is_invalid() {
            unsafe {
                PdhCloseQuery(self.query);
            }
        }
    }
}
impl Sampler {
    // Reset the Windows state
    pub fn reset(&mut self) {
        self.warmed = false;
    }
    // Collect a Windows sample
    pub fn sample(&mut self, pids: &BTreeSet<u32>) -> Reading {
        unsafe {
            if self.query.is_invalid() {
                if PdhOpenQueryW(PCWSTR::null(), 0, &mut self.query) != 0 {
                    return Reading::unavailable();
                }
                if PdhAddEnglishCounterW(
                    self.query,
                    w!("\\GPU Engine(*)\\Utilization Percentage"),
                    0,
                    &mut self.counter,
                ) != 0
                {
                    PdhCloseQuery(self.query);
                    self.query = PDH_HQUERY::default();
                    return Reading::unavailable();
                }
            }
            if PdhCollectQueryData(self.query) != 0 {
                self.warmed = false;
                return Reading::unavailable();
            }
            if !self.warmed {
                self.warmed = true;
                return Reading::warming();
            }
            let mut bytes = 0;
            let mut count = 0;
            if PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                None,
            ) != PDH_MORE_DATA
                || bytes > 4 * 1024 * 1024
            {
                return Reading::unavailable();
            }
            // u64 storage supplies the alignment required by PDH's item structures
            let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
            if PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut bytes,
                &mut count,
                Some(buffer.as_mut_ptr().cast()),
            ) != 0
                || count as usize
                    > buffer.len() * 8 / std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>()
            {
                return Reading::unavailable();
            }
            let items = std::slice::from_raw_parts(
                buffer.as_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>(),
                count as usize,
            );
            let mut engines = BTreeMap::<String, f64>::new();
            let mut valid = false;
            for item in items {
                if ![PDH_CSTATUS_VALID_DATA, PDH_CSTATUS_NEW_DATA].contains(&item.FmtValue.CStatus)
                {
                    continue;
                }
                let value = item.FmtValue.Anonymous.doubleValue;
                if !value.is_finite() || value < 0. {
                    continue;
                }
                valid = true;
                let Ok(name) = item.szName.to_string() else {
                    continue;
                };
                let Some(rest) = name.strip_prefix("pid_") else {
                    continue;
                };
                let Some((pid, engine)) = rest.split_once('_') else {
                    continue;
                };
                if pid.parse::<u32>().is_ok_and(|pid| pids.contains(&pid)) {
                    *engines.entry(engine.to_owned()).or_default() += value;
                }
            }
            if valid {
                Reading::available(engines.values().copied().fold(0., f64::max))
            } else {
                Reading::unavailable()
            }
        }
    }
}
