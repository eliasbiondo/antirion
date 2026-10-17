//! A ZIP archive of stored (uncompressed) entries with UTF-8 names, built in memory: what an export
//! job hands out. Archives and entries stay below 4 GiB, so no ZIP64 record is needed.

/// Why an archive could not be built.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TooLarge;

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
/// Version 2.0: the oldest that every reader accepts for stored entries.
const VERSION: u16 = 20;
/// General-purpose flag 11: names are UTF-8.
const UTF8: u16 = 0x0800;
/// 1980-01-01 00:00:00 in MS-DOS date and time, the earliest such a header can hold.
const DOS_DATE: u16 = (1 << 5) | 1;
const DOS_TIME: u16 = 0;

struct Entry {
    name: Vec<u8>,
    crc: u32,
    size: u32,
    offset: u32,
}

/// An archive being written.
#[derive(Default)]
pub(crate) struct ZipWriter {
    out: Vec<u8>,
    entries: Vec<Entry>,
}

fn u16_of(n: usize) -> Result<u16, TooLarge> {
    u16::try_from(n).map_err(|_| TooLarge)
}

fn u32_of(n: usize) -> Result<u32, TooLarge> {
    u32::try_from(n).map_err(|_| TooLarge)
}

impl ZipWriter {
    /// Adds a file named `name` holding `data`.
    pub(crate) fn add(&mut self, name: &str, data: &[u8]) -> Result<(), TooLarge> {
        let entry = Entry {
            name: name.as_bytes().to_vec(),
            crc: crc32fast::hash(data),
            size: u32_of(data.len())?,
            offset: u32_of(self.out.len())?,
        };
        let o = &mut self.out;
        o.extend_from_slice(&LOCAL.to_le_bytes());
        for v in [VERSION, UTF8, 0, DOS_TIME, DOS_DATE] {
            o.extend_from_slice(&v.to_le_bytes());
        }
        for v in [entry.crc, entry.size, entry.size] {
            o.extend_from_slice(&v.to_le_bytes());
        }
        o.extend_from_slice(&u16_of(entry.name.len())?.to_le_bytes());
        o.extend_from_slice(&0u16.to_le_bytes());
        o.extend_from_slice(&entry.name);
        o.extend_from_slice(data);
        u32_of(o.len())?;
        self.entries.push(entry);
        Ok(())
    }

    /// The archive: the entries, their central directory and its end record.
    pub(crate) fn finish(mut self) -> Result<Vec<u8>, TooLarge> {
        let start = u32_of(self.out.len())?;
        let o = &mut self.out;
        for e in &self.entries {
            o.extend_from_slice(&CENTRAL.to_le_bytes());
            for v in [VERSION, VERSION, UTF8, 0, DOS_TIME, DOS_DATE] {
                o.extend_from_slice(&v.to_le_bytes());
            }
            for v in [e.crc, e.size, e.size] {
                o.extend_from_slice(&v.to_le_bytes());
            }
            for v in [u16_of(e.name.len())?, 0, 0, 0, 0] {
                o.extend_from_slice(&v.to_le_bytes());
            }
            o.extend_from_slice(&0u32.to_le_bytes());
            o.extend_from_slice(&e.offset.to_le_bytes());
            o.extend_from_slice(&e.name);
        }
        let size = u32_of(o.len())? - start;
        let count = u16_of(self.entries.len())?;
        o.extend_from_slice(&END.to_le_bytes());
        for v in [0, 0, count, count] {
            o.extend_from_slice(&v.to_le_bytes());
        }
        o.extend_from_slice(&size.to_le_bytes());
        o.extend_from_slice(&start.to_le_bytes());
        o.extend_from_slice(&0u16.to_le_bytes());
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(b: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([b[at], b[at + 1]])
    }

    fn u32_at(b: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    #[test]
    fn entries_are_found_through_the_central_directory() {
        let mut z = ZipWriter::default();
        z.add("manifest.json", b"{}").unwrap();
        z.add("config.json", b"{\"a\":1}").unwrap();
        let bytes = z.finish().unwrap();
        let end = bytes.len() - 22;
        assert_eq!(u32_at(&bytes, end), END);
        assert_eq!(u16_at(&bytes, end + 10), 2);
        let mut at = u32_at(&bytes, end + 16) as usize;
        let mut names = Vec::new();
        for _ in 0..2 {
            assert_eq!(u32_at(&bytes, at), CENTRAL);
            let size = u32_at(&bytes, at + 24) as usize;
            let name_len = u16_at(&bytes, at + 28) as usize;
            let offset = u32_at(&bytes, at + 42) as usize;
            let name = String::from_utf8(bytes[at + 46..at + 46 + name_len].to_vec()).unwrap();
            assert_eq!(u32_at(&bytes, offset), LOCAL);
            let data_at = offset + 30 + u16_at(&bytes, offset + 26) as usize;
            let data = &bytes[data_at..data_at + size];
            assert_eq!(u32_at(&bytes, at + 16), crc32fast::hash(data));
            names.push((name, String::from_utf8(data.to_vec()).unwrap()));
            at += 46 + name_len;
        }
        assert_eq!(
            names,
            vec![
                ("manifest.json".to_owned(), "{}".to_owned()),
                ("config.json".to_owned(), "{\"a\":1}".to_owned())
            ]
        );
    }
}
