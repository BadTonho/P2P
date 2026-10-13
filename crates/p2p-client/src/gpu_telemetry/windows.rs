use super::*;
use windows::Win32::System::Performance::*;
use windows::core::{PCWSTR, w};

pub(super) struct NativeSampler {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
}

impl NativeSampler {
    pub fn new() -> Result<Self, String> {
        let mut query = PDH_HQUERY::default();
        check(
            unsafe { PdhOpenQueryW(PCWSTR::null(), 0, &mut query) },
            "open_query",
        )?;
        let mut provider = Self {
            query,
            counter: PDH_HCOUNTER::default(),
        };
        check(
            unsafe {
                PdhAddEnglishCounterW(
                    query,
                    w!("\\GPU Engine(*)\\Utilization Percentage"),
                    0,
                    &mut provider.counter,
                )
            },
            "add_gpu_engine_counter",
        )?;
        check(unsafe { PdhCollectQueryData(query) }, "initial_sample")?;
        Ok(provider)
    }
}

fn check(status: u32, stage: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "counter_unavailable: {stage}, status=0x{status:08x}; percentage=unavailable"
        ))
    }
}

impl Drop for NativeSampler {
    fn drop(&mut self) {
        unsafe {
            PdhCloseQuery(self.query);
        }
    }
}

impl Sampler for NativeSampler {
    fn sample(&mut self) -> Result<BTreeMap<String, f64>, String> {
        check(unsafe { PdhCollectQueryData(self.query) }, "collect")?;
        for _ in 0..2 {
            let mut bytes = 0;
            let mut count = 0;
            let status = unsafe {
                PdhGetFormattedCounterArrayW(
                    self.counter,
                    PDH_FMT_DOUBLE,
                    &mut bytes,
                    &mut count,
                    None,
                )
            };
            if status != PDH_MORE_DATA {
                check(status, "array_size")?;
                return Ok(BTreeMap::new());
            }
            if bytes as usize > 2 * 1024 * 1024 {
                return Err("counter_buffer_limit; percentage=unavailable".to_owned());
            }
            let elements =
                (bytes as usize).div_ceil(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>());
            let mut storage = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); elements];
            let status = unsafe {
                PdhGetFormattedCounterArrayW(
                    self.counter,
                    PDH_FMT_DOUBLE,
                    &mut bytes,
                    &mut count,
                    Some(storage.as_mut_ptr()),
                )
            };
            if status == PDH_MORE_DATA {
                continue;
            }
            check(status, "array_data")?;
            return counter_values(&storage, bytes as usize, count as usize, std::process::id());
        }
        Err("counter_instances_changed; percentage=unavailable".to_owned())
    }
}

fn counter_values(
    storage: &[PDH_FMT_COUNTERVALUE_ITEM_W],
    bytes: usize,
    count: usize,
    pid: u32,
) -> Result<BTreeMap<String, f64>, String> {
    let item_bytes = count.checked_mul(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>());
    if bytes > std::mem::size_of_val(storage) || item_bytes.is_none_or(|size| size > bytes) {
        return Err("invalid_counter_buffer; percentage=unavailable".to_owned());
    }
    let mut result = BTreeMap::new();
    for item in &storage[..count] {
        if !matches!(
            item.FmtValue.CStatus,
            PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
        ) {
            continue;
        }
        // PDH puts names inside the caller's aligned buffer. Bound the UTF-16 read to it.
        let base = storage.as_ptr() as usize;
        let name = item.szName.0 as usize;
        if name < base + item_bytes.unwrap() || name >= base + bytes || !name.is_multiple_of(2) {
            continue;
        }
        let available = (base + bytes - name) / 2;
        let wide = unsafe { std::slice::from_raw_parts(item.szName.0, available) };
        let Some(length) = wide.iter().position(|value| *value == 0) else {
            continue;
        };
        let instance = String::from_utf16_lossy(&wide[..length]);
        let percentage = unsafe { item.FmtValue.Anonymous.doubleValue };
        if let Some((engine, percentage)) = process_engine(&instance, pid, percentage) {
            // If the provider repeats an identity, retain its maximum, never sum it.
            result
                .entry(engine)
                .and_modify(|value: &mut f64| *value = value.max(percentage))
                .or_insert(percentage);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(instances: &[(&str, f64, u32)]) -> (Vec<PDH_FMT_COUNTERVALUE_ITEM_W>, usize) {
        let names = instances
            .iter()
            .map(|(name, _, _)| name.encode_utf16().chain([0]).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let headers = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>() * instances.len();
        let bytes = headers + names.iter().map(|name| name.len() * 2).sum::<usize>();
        let mut storage = vec![
            PDH_FMT_COUNTERVALUE_ITEM_W::default();
            bytes.div_ceil(std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>())
        ];
        let mut offset = headers;
        for (index, ((_, percentage, status), name)) in instances.iter().zip(names).enumerate() {
            let pointer = unsafe { storage.as_mut_ptr().cast::<u8>().add(offset).cast::<u16>() };
            // Same aligned caller-owned layout returned by PDH, without consulting hardware.
            unsafe {
                std::ptr::copy_nonoverlapping(name.as_ptr(), pointer, name.len());
            }
            storage[index].szName = windows::core::PWSTR(pointer);
            storage[index].FmtValue.CStatus = *status;
            storage[index].FmtValue.Anonymous.doubleValue = *percentage;
            offset += name.len() * 2;
        }
        (storage, bytes)
    }

    #[test]
    fn formatted_buffer_filters_invalid_samples_and_deduplicates_without_summing() {
        let name = "pid_12_luid_0x0_0x123_phys_0_eng_1_engtype_VideoDecode";
        let (storage, bytes) = buffer(&[
            (name, 20.0, PDH_CSTATUS_VALID_DATA),
            (name, 30.0, PDH_CSTATUS_NEW_DATA),
            (name, 99.0, PDH_CSTATUS_INVALID_DATA),
            (
                "pid_123_luid_0x0_0x123_phys_0_eng_1_engtype_VideoEncode",
                80.0,
                PDH_CSTATUS_VALID_DATA,
            ),
        ]);
        let result = counter_values(&storage, bytes, 4, 12).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.values().next(), Some(&30.0));
    }

    #[test]
    fn formatted_buffer_rejects_truncation_and_bounds_string_reads() {
        let name = "pid_12_luid_0x0_0x123_phys_0_eng_1_engtype_VideoDecode";
        let (mut storage, bytes) = buffer(&[(name, 25.0, PDH_CSTATUS_VALID_DATA)]);
        assert!(
            counter_values(
                &storage,
                std::mem::size_of_val(storage.as_slice()) + 1,
                1,
                12
            )
            .is_err()
        );
        assert!(counter_values(&storage, 1, 1, 12).is_err());
        assert!(counter_values(&storage, bytes, usize::MAX, 12).is_err());
        // Excluding the final terminator must not read past the buffer.
        assert!(
            counter_values(&storage, bytes - 2, 1, 12)
                .unwrap()
                .is_empty()
        );
        storage[0].szName = windows::core::PWSTR::null();
        assert!(counter_values(&storage, bytes, 1, 12).unwrap().is_empty());
        storage[0].szName = windows::core::PWSTR(storage.as_mut_ptr().cast::<u16>());
        assert!(counter_values(&storage, bytes, 1, 12).unwrap().is_empty());
        storage[0].szName = windows::core::PWSTR(
            storage
                .as_mut_ptr()
                .cast::<u8>()
                .wrapping_add(bytes + 1)
                .cast::<u16>(),
        );
        assert!(counter_values(&storage, bytes, 1, 12).unwrap().is_empty());
    }
}
