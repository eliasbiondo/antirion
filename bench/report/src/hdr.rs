//! Histograms on disk: the HdrHistogram interval-log format (V2 compressed), one interval per file.

use std::path::Path;
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hdrhistogram::Histogram;
use hdrhistogram::serialization::interval_log::{IntervalLogIterator, IntervalLogWriterBuilder, LogEntry};
use hdrhistogram::serialization::{Deserializer, V2DeflateSerializer};

use crate::ReportError;

/// Writes `h` as one interval of `duration` starting at `start` (the window's wall-clock start).
pub fn write(
    path: &Path,
    h: &Histogram<u64>,
    start: SystemTime,
    duration: Duration,
) -> Result<(), ReportError> {
    let mut buf = Vec::new();
    let mut ser = V2DeflateSerializer::new();
    let io = |e: std::io::Error| ReportError::Write {
        path: path.to_owned(),
        source: e,
    };
    let mut writer = IntervalLogWriterBuilder::new()
        .add_comment("latency in microseconds")
        .with_start_time(start)
        .with_base_time(start)
        .with_max_value_divisor(1.0)
        .begin_log_with(&mut buf, &mut ser)
        .map_err(io)?;
    writer
        .write_histogram(h, Duration::ZERO, duration, None)
        .map_err(|e| ReportError::Hdr(format!("{e:?}")))?;
    std::fs::write(path, buf).map_err(io)
}

/// Reads every interval of the log at `path`, added into one histogram.
pub fn read(path: &Path) -> Result<Histogram<u64>, ReportError> {
    let bytes = std::fs::read(path).map_err(|source| ReportError::Read {
        path: path.to_owned(),
        source,
    })?;
    let mut total: Option<Histogram<u64>> = None;
    let mut de = Deserializer::new();
    for entry in IntervalLogIterator::new(&bytes) {
        let entry = entry.map_err(|e| ReportError::Hdr(format!("{}: {e:?}", path.display())))?;
        if let LogEntry::Interval(iv) = entry {
            let raw = STANDARD
                .decode(iv.encoded_histogram())
                .map_err(|e| ReportError::Hdr(format!("{}: {e}", path.display())))?;
            let h: Histogram<u64> = de
                .deserialize(&mut raw.as_slice())
                .map_err(|e| ReportError::Hdr(format!("{}: {e:?}", path.display())))?;
            match &mut total {
                Some(t) => t
                    .add(&h)
                    .map_err(|e| ReportError::Hdr(format!("{}: {e:?}", path.display())))?,
                None => total = Some(h),
            }
        }
    }
    total.ok_or_else(|| ReportError::Hdr(format!("{} holds no histogram", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("hdr-rt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.hdr");
        let mut h = Histogram::<u64>::new_with_bounds(1, 3_600_000_000, 3).unwrap();
        for v in [100, 200, 300, 4_000, 1_500_000] {
            h.record(v).unwrap();
        }
        write(&path, &h, SystemTime::UNIX_EPOCH, Duration::from_secs(120)).unwrap();
        let back = read(&path).unwrap();
        assert_eq!(back.len(), 5);
        assert_eq!(back.max(), h.max());
        assert_eq!(back.value_at_quantile(0.5), h.value_at_quantile(0.5));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
