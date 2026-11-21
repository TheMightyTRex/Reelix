//! The OpenSubtitles file hash: the file size plus the sum of the first and the last 64 KiB
//! read as little-endian `u64`s, wrapping on overflow, written as 16 lower-case hex digits.
//! Subtitles uploaded with the same hash were made for this exact file.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes read from each end of the file.
pub const CHUNK: u64 = 64 * 1024;

/// The hash of `path` (blocking file reads; call from `spawn_blocking`).
pub fn file_hash(path: &Path) -> std::io::Result<(String, u64)> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    Ok((hash_reader(&mut f, size)?, size))
}

/// The hash of a reader of `size` bytes.
pub fn hash_reader<R: Read + Seek>(r: &mut R, size: u64) -> std::io::Result<String> {
    let len = CHUNK.min(size) as usize;
    let mut head = vec![0u8; len];
    r.seek(SeekFrom::Start(0))?;
    r.read_exact(&mut head)?;
    let mut tail = vec![0u8; len];
    r.seek(SeekFrom::Start(size - len as u64))?;
    r.read_exact(&mut tail)?;
    Ok(format!("{:016x}", hash_parts(size, &head, &tail)))
}

/// The hash from the size and the two ends.
pub fn hash_parts(size: u64, head: &[u8], tail: &[u8]) -> u64 {
    sum_le_u64(tail, sum_le_u64(head, size))
}

fn sum_le_u64(bytes: &[u8], start: u64) -> u64 {
    bytes.chunks(8).fold(start, |acc, c| {
        let mut word = [0u8; 8];
        word[..c.len()].copy_from_slice(c);
        acc.wrapping_add(u64::from_le_bytes(word))
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;

    /// A file of zeros hashes to its size: the published algorithm adds nothing else.
    #[test]
    fn zeros_hash_to_their_size() {
        let size = 3 * CHUNK + 5;
        let mut c = Cursor::new(vec![0u8; size as usize]);
        assert_eq!(hash_reader(&mut c, size).unwrap(), format!("{size:016x}"));
    }

    /// Reference value computed with the algorithm as OpenSubtitles publishes it (their Python
    /// sample: `struct.unpack('<q')` over 8192 words from each end, masked to 64 bits) over the
    /// same synthetic bytes: byte i = (i * 7 + 3) mod 251, 300,000 bytes.
    #[test]
    fn matches_reference_implementation() {
        let data: Vec<u8> = (0..300_000u32).map(|i| ((i * 7 + 3) % 251) as u8).collect();
        let size = data.len() as u64;
        let mut c = Cursor::new(data);
        let reference = include_str!("../tests/fixtures/hash-reference.txt").trim();
        assert_eq!(hash_reader(&mut c, size).unwrap(), reference);
    }

    /// The two ends overlap in a file shorter than 128 KiB; each is still read in full.
    #[test]
    fn small_file_reads_overlapping_ends() {
        let data = vec![1u8; 10];
        let mut c = Cursor::new(data.clone());
        assert_eq!(
            hash_reader(&mut c, 10).unwrap(),
            format!("{:016x}", hash_parts(10, &data, &data))
        );
    }

    #[test]
    fn sums_wrap_instead_of_overflowing() {
        let head = u64::MAX.to_le_bytes();
        assert_eq!(hash_parts(2, &head, &[]), 1);
    }

    #[test]
    fn file_hash_reads_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("v.mkv");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(&[0u8; 1000])
            .unwrap();
        assert_eq!(file_hash(&p).unwrap(), (format!("{:016x}", 1000), 1000));
    }

    /// The published test vector: `breakdance.avi` (12,909,756 bytes) hashes to
    /// `8e245d9679d31e12`. The sample file is not kept in the repository; run with
    /// `REELIX_OSHASH_SAMPLE=/path/to/breakdance.avi cargo test -p reelix-subtitles -- --ignored`.
    #[test]
    #[ignore = "needs the OpenSubtitles sample file breakdance.avi"]
    fn published_test_vector() {
        let p = std::env::var("REELIX_OSHASH_SAMPLE").expect("set REELIX_OSHASH_SAMPLE");
        let (hash, size) = file_hash(Path::new(&p)).unwrap();
        assert_eq!(size, 12_909_756);
        assert_eq!(hash, "8e245d9679d31e12");
    }
}
