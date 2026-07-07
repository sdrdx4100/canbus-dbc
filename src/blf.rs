//! Streaming reader for the Vector Binary Logging Format (BLF).
//!
//! Supports the object types needed for CAN decoding:
//! - `LOG_CONTAINER` (10), optionally zlib-compressed
//! - `CAN_MESSAGE` (1) / `CAN_MESSAGE2` (86)
//! - `CAN_FD_MESSAGE` (100) / `CAN_FD_MESSAGE_64` (101)
//!
//! Everything else is skipped. Objects are parsed out of container payloads
//! by scanning for the `LOBJ` signature, so unknown padding conventions do
//! not derail the reader.

use anyhow::{Context, Result, bail};
use flate2::read::ZlibDecoder;
use std::collections::VecDeque;
use std::io::Read;

const FILE_SIGNATURE: &[u8; 4] = b"LOGG";
const OBJ_SIGNATURE: &[u8; 4] = b"LOBJ";

const OBJ_TYPE_CAN_MESSAGE: u32 = 1;
const OBJ_TYPE_LOG_CONTAINER: u32 = 10;
const OBJ_TYPE_CAN_MESSAGE2: u32 = 86;
const OBJ_TYPE_CAN_FD_MESSAGE: u32 = 100;
const OBJ_TYPE_CAN_FD_MESSAGE_64: u32 = 101;

const COMPRESSION_NONE: u16 = 0;
const COMPRESSION_ZLIB: u16 = 2;

/// Timestamp flag bits in the object header.
const FLAG_TIME_TEN_MICS: u32 = 1;
const FLAG_TIME_ONE_NANS: u32 = 2;

const CAN_ID_EXTENDED_BIT: u32 = 0x8000_0000;

/// A decoded CAN / CAN FD frame from the log.
#[derive(Debug, Clone)]
pub struct CanFrame {
    /// Nanoseconds since measurement start.
    pub timestamp_ns: u64,
    pub channel: u16,
    /// 11-bit or 29-bit identifier (extended bit stripped).
    pub id: u32,
    pub is_extended: bool,
    pub data: [u8; 64],
    pub len: u8,
    /// Remote frames carry no data and are skipped by the decoder.
    pub is_remote: bool,
}

impl CanFrame {
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len as usize]
    }
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}
fn u64le(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// Convert a CAN FD DLC to the payload length in bytes.
fn fd_dlc_to_len(dlc: u8) -> u8 {
    match dlc {
        0..=8 => dlc,
        9 => 12,
        10 => 16,
        11 => 20,
        12 => 24,
        13 => 32,
        14 => 48,
        _ => 64,
    }
}

/// Wall-clock time of measurement start, extracted from the file header.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemTimeFields {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
    pub millisecond: u16,
}

impl SystemTimeFields {
    fn parse(b: &[u8]) -> Self {
        Self {
            year: u16le(&b[0..]),
            month: u16le(&b[2..]),
            // b[4..6] is day-of-week, skipped
            day: u16le(&b[6..]),
            hour: u16le(&b[8..]),
            minute: u16le(&b[10..]),
            second: u16le(&b[12..]),
            millisecond: u16le(&b[14..]),
        }
    }

    /// Seconds since the Unix epoch (UTC assumed), or 0.0 if unset.
    pub fn to_epoch_seconds(&self) -> f64 {
        if self.year == 0 {
            return 0.0;
        }
        // Days since epoch via civil-date algorithm (Howard Hinnant).
        let y = self.year as i64 - if self.month <= 2 { 1 } else { 0 };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let m = self.month as i64;
        let d = self.day as i64;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146097 + doe - 719468;
        days as f64 * 86400.0
            + self.hour as f64 * 3600.0
            + self.minute as f64 * 60.0
            + self.second as f64
            + self.millisecond as f64 / 1000.0
    }
}

struct CountingReader<R: Read> {
    inner: R,
    bytes_read: u64,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read += n as u64;
        Ok(n)
    }
}

/// Streaming BLF reader. Reads compressed containers one at a time, so peak
/// memory stays proportional to a single container regardless of file size.
pub struct BlfReader<R: Read> {
    reader: CountingReader<R>,
    /// Decompressed container bytes not yet fully parsed.
    buffer: Vec<u8>,
    /// Parse position inside `buffer`.
    pos: usize,
    frames: VecDeque<CanFrame>,
    pub start_time: SystemTimeFields,
    eof: bool,
}

impl<R: Read> BlfReader<R> {
    pub fn new(reader: R) -> Result<Self> {
        let mut reader = CountingReader {
            inner: reader,
            bytes_read: 0,
        };
        let mut head = [0u8; 8];
        reader
            .read_exact(&mut head)
            .context("BLF file header is truncated")?;
        if &head[0..4] != FILE_SIGNATURE {
            bail!("not a BLF file (missing LOGG signature)");
        }
        let header_size = u32le(&head[4..]) as usize;
        if !(8..=0x10000).contains(&header_size) {
            bail!("invalid BLF header size: {header_size}");
        }
        let mut rest = vec![0u8; header_size - 8];
        reader
            .read_exact(&mut rest)
            .context("BLF file header is truncated")?;
        // Offsets relative to file start: measurement start SYSTEMTIME at 56.
        let start_time = if header_size >= 72 + 16 {
            SystemTimeFields::parse(&rest[48..64])
        } else {
            SystemTimeFields::default()
        };
        Ok(Self {
            reader,
            buffer: Vec::new(),
            pos: 0,
            frames: VecDeque::new(),
            start_time,
            eof: false,
        })
    }

    /// Compressed bytes consumed so far (for progress reporting).
    pub fn bytes_read(&self) -> u64 {
        self.reader.bytes_read
    }

    /// Next CAN frame, or `None` at end of file.
    pub fn next_frame(&mut self) -> Result<Option<CanFrame>> {
        loop {
            if let Some(frame) = self.frames.pop_front() {
                return Ok(Some(frame));
            }
            if self.eof {
                return Ok(None);
            }
            self.read_top_level_object()?;
        }
    }

    /// Read one object from the underlying file. Containers are inflated and
    /// their contents parsed into `self.frames`.
    fn read_top_level_object(&mut self) -> Result<()> {
        let mut base = [0u8; 16];
        // Tolerate clean EOF and trailing padding bytes between objects.
        match read_exact_or_eof(&mut self.reader, &mut base)? {
            ReadOutcome::Full => {}
            ReadOutcome::Eof | ReadOutcome::Partial => {
                self.eof = true;
                return Ok(());
            }
        }
        if &base[0..4] != OBJ_SIGNATURE {
            // Skip byte-by-byte until the next LOBJ signature.
            if !self.resync(&mut base)? {
                self.eof = true;
                return Ok(());
            }
        }
        let header_size = u16le(&base[4..]) as usize;
        let object_size = u32le(&base[8..]) as usize;
        let object_type = u32le(&base[12..]);
        if object_size < 16 || header_size < 16 {
            bail!("corrupt BLF object header (size {object_size})");
        }
        let mut body = vec![0u8; object_size - 16];
        self.reader
            .read_exact(&mut body)
            .context("BLF object is truncated")?;

        if object_type == OBJ_TYPE_LOG_CONTAINER {
            self.consume_container(header_size, &body)?;
        } else {
            let mut obj = Vec::with_capacity(object_size);
            obj.extend_from_slice(&base);
            obj.extend_from_slice(&body);
            if let Some(frame) = parse_message_object(&obj)? {
                self.frames.push_back(frame);
            }
        }
        // Top-level objects are padded to 4-byte alignment.
        let padding = object_size % 4;
        if padding != 0 {
            let mut pad = [0u8; 4];
            let _ = self.reader.read(&mut pad[..padding])?;
        }
        Ok(())
    }

    /// Scan forward for the next `LOBJ` signature, refilling `base`.
    fn resync(&mut self, base: &mut [u8; 16]) -> Result<bool> {
        let mut window = [base[1], base[2], base[3]];
        loop {
            let mut byte = [0u8; 1];
            if self.reader.read(&mut byte)? == 0 {
                return Ok(false);
            }
            if window == [b'L', b'O', b'B'] && byte[0] == b'J' {
                base[0..4].copy_from_slice(OBJ_SIGNATURE);
                return match read_exact_or_eof(&mut self.reader, &mut base[4..])? {
                    ReadOutcome::Full => Ok(true),
                    ReadOutcome::Eof | ReadOutcome::Partial => Ok(false),
                };
            }
            window = [window[1], window[2], byte[0]];
        }
    }

    /// Inflate a LOG_CONTAINER body and parse the objects inside it.
    fn consume_container(&mut self, header_size: usize, body: &[u8]) -> Result<()> {
        // Container-specific fields follow the base header:
        // compression method u16, 6 reserved, uncompressed size u32, 4 reserved.
        let fields_start = header_size.saturating_sub(16);
        if body.len() < fields_start + 16 {
            bail!("corrupt LOG_CONTAINER object");
        }
        let method = u16le(&body[fields_start..]);
        let uncompressed_size = u32le(&body[fields_start + 8..]) as usize;
        let payload = &body[fields_start + 16..];

        // Keep unparsed tail from the previous container: an object may span
        // container boundaries.
        self.buffer.drain(..self.pos);
        self.pos = 0;

        match method {
            COMPRESSION_NONE => self.buffer.extend_from_slice(payload),
            COMPRESSION_ZLIB => {
                self.buffer.reserve(uncompressed_size);
                let mut decoder = ZlibDecoder::new(payload);
                decoder
                    .read_to_end(&mut self.buffer)
                    .context("failed to inflate BLF log container")?;
            }
            other => bail!("unsupported BLF container compression method {other}"),
        }
        self.parse_buffer()
    }

    /// Parse complete objects out of `self.buffer`, leaving any partial
    /// object at the tail for the next container.
    fn parse_buffer(&mut self) -> Result<()> {
        loop {
            // Find the next object signature from the current position.
            let Some(rel) = find_signature(&self.buffer[self.pos..]) else {
                // Keep at most 3 bytes: a split signature at the very tail.
                self.pos = self.buffer.len().saturating_sub(3).max(self.pos);
                return Ok(());
            };
            let start = self.pos + rel;
            if self.buffer.len() - start < 16 {
                self.pos = start;
                return Ok(());
            }
            let object_size = u32le(&self.buffer[start + 8..]) as usize;
            if object_size < 16 {
                // Corrupt entry; skip past this signature and keep scanning.
                self.pos = start + 4;
                continue;
            }
            if self.buffer.len() - start < object_size {
                self.pos = start;
                return Ok(());
            }
            let obj = &self.buffer[start..start + object_size];
            if let Some(frame) = parse_message_object(obj)? {
                self.frames.push_back(frame);
            }
            self.pos = start + object_size;
        }
    }
}

fn find_signature(haystack: &[u8]) -> Option<usize> {
    haystack
        .windows(4)
        .position(|window| window == OBJ_SIGNATURE)
}

enum ReadOutcome {
    Full,
    /// EOF before any byte was read.
    Eof,
    /// EOF partway through the buffer (trailing padding / truncated tail).
    Partial,
}

/// Read exactly `buf.len()` bytes, reporting EOF instead of erroring.
/// I/O errors other than EOF still propagate.
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<ReadOutcome> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..])?;
        if n == 0 {
            return Ok(if filled == 0 {
                ReadOutcome::Eof
            } else {
                ReadOutcome::Partial
            });
        }
        filled += n;
    }
    Ok(ReadOutcome::Full)
}

/// Parse a complete object (base header included). Returns a frame for CAN
/// message object types, `None` for everything else.
fn parse_message_object(obj: &[u8]) -> Result<Option<CanFrame>> {
    let header_size = u16le(&obj[4..]) as usize;
    let header_version = u16le(&obj[6..]);
    let object_type = u32le(&obj[12..]);

    let is_can = matches!(
        object_type,
        OBJ_TYPE_CAN_MESSAGE
            | OBJ_TYPE_CAN_MESSAGE2
            | OBJ_TYPE_CAN_FD_MESSAGE
            | OBJ_TYPE_CAN_FD_MESSAGE_64
    );
    if !is_can {
        return Ok(None);
    }
    if obj.len() < header_size {
        bail!("BLF object shorter than its header");
    }

    // Extended header (V1: flags u32, client u16, version u16, timestamp u64;
    // V2: flags u32, status u8, reserved u8, version u16, timestamp u64, ...).
    let ext = &obj[16..];
    let (flags, timestamp) = match header_version {
        1 => (u32le(ext), u64le(&ext[8..])),
        2 => (u32le(ext), u64le(&ext[8..])),
        v => bail!("unsupported BLF object header version {v}"),
    };
    let timestamp_ns = if flags & FLAG_TIME_ONE_NANS != 0 {
        timestamp
    } else if flags & FLAG_TIME_TEN_MICS != 0 {
        timestamp * 10_000
    } else {
        timestamp
    };

    let body = &obj[header_size..];
    let frame = match object_type {
        OBJ_TYPE_CAN_MESSAGE | OBJ_TYPE_CAN_MESSAGE2 => {
            // channel u16, flags u8, dlc u8, id u32, data[8]
            if body.len() < 16 {
                return Ok(None);
            }
            let channel = u16le(body);
            let msg_flags = body[2];
            let dlc = body[3];
            let raw_id = u32le(&body[4..]);
            let len = dlc.min(8);
            let mut data = [0u8; 64];
            data[..8].copy_from_slice(&body[8..16]);
            CanFrame {
                timestamp_ns,
                channel,
                id: raw_id & !CAN_ID_EXTENDED_BIT,
                is_extended: raw_id & CAN_ID_EXTENDED_BIT != 0,
                data,
                len,
                is_remote: msg_flags & 0x80 != 0,
            }
        }
        OBJ_TYPE_CAN_FD_MESSAGE => {
            // channel u16, flags u8, dlc u8, id u32, frame_length u32,
            // bit_count u8, fd_flags u8, valid_bytes u8, 5 reserved, data[64]
            if body.len() < 15 {
                return Ok(None);
            }
            let channel = u16le(body);
            let msg_flags = body[2];
            let dlc = body[3];
            let raw_id = u32le(&body[4..]);
            let valid_bytes = body[14];
            let len = fd_dlc_to_len(dlc).min(if valid_bytes > 0 { valid_bytes } else { 64 });
            let mut data = [0u8; 64];
            let avail = body.len().saturating_sub(20).min(len as usize);
            data[..avail].copy_from_slice(&body[20..20 + avail]);
            CanFrame {
                timestamp_ns,
                channel,
                id: raw_id & !CAN_ID_EXTENDED_BIT,
                is_extended: raw_id & CAN_ID_EXTENDED_BIT != 0,
                data,
                len,
                is_remote: msg_flags & 0x80 != 0,
            }
        }
        OBJ_TYPE_CAN_FD_MESSAGE_64 => {
            // channel u8, dlc u8, valid_payload_len u8, tx_count u8, id u32,
            // frame_length u32, flags u32, ..., data at offset 40.
            if body.len() < 40 {
                return Ok(None);
            }
            let channel = body[0] as u16;
            let dlc = body[1];
            let valid_bytes = body[2];
            let raw_id = u32le(&body[4..]);
            let fd_flags = u32le(&body[12..]);
            let len = fd_dlc_to_len(dlc).min(if valid_bytes > 0 { valid_bytes } else { 64 });
            let mut data = [0u8; 64];
            let avail = body.len().saturating_sub(40).min(len as usize);
            data[..avail].copy_from_slice(&body[40..40 + avail]);
            CanFrame {
                timestamp_ns,
                channel,
                id: raw_id & !CAN_ID_EXTENDED_BIT,
                is_extended: raw_id & CAN_ID_EXTENDED_BIT != 0,
                data,
                len,
                // bit 4 of the FD flags marks a remote frame
                is_remote: fd_flags & 0x0010 != 0,
            }
        }
        _ => unreachable!(),
    };
    Ok(Some(frame))
}

#[cfg(test)]
pub mod test_support {
    //! Helpers to synthesize BLF byte streams for tests.
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;

    pub fn file_header() -> Vec<u8> {
        let mut h = vec![0u8; 144];
        h[0..4].copy_from_slice(FILE_SIGNATURE);
        h[4..8].copy_from_slice(&144u32.to_le_bytes());
        // measurement start SYSTEMTIME at offset 56: 2024-01-02 03:04:05.678
        let st: [u16; 8] = [2024, 1, 2, 2, 3, 4, 5, 678];
        for (i, v) in st.iter().enumerate() {
            h[56 + i * 2..58 + i * 2].copy_from_slice(&v.to_le_bytes());
        }
        h
    }

    /// Build a CAN_MESSAGE object (type 1) with a V1 header, ns timestamps.
    pub fn can_message(timestamp_ns: u64, id: u32, extended: bool, data: &[u8]) -> Vec<u8> {
        let mut obj = Vec::new();
        obj.extend_from_slice(OBJ_SIGNATURE);
        obj.extend_from_slice(&32u16.to_le_bytes()); // header size
        obj.extend_from_slice(&1u16.to_le_bytes()); // header version
        obj.extend_from_slice(&48u32.to_le_bytes()); // object size
        obj.extend_from_slice(&OBJ_TYPE_CAN_MESSAGE.to_le_bytes());
        obj.extend_from_slice(&FLAG_TIME_ONE_NANS.to_le_bytes()); // flags
        obj.extend_from_slice(&0u16.to_le_bytes()); // client index
        obj.extend_from_slice(&0u16.to_le_bytes()); // object version
        obj.extend_from_slice(&timestamp_ns.to_le_bytes());
        let raw_id = if extended {
            id | CAN_ID_EXTENDED_BIT
        } else {
            id
        };
        obj.extend_from_slice(&1u16.to_le_bytes()); // channel
        obj.push(0); // flags
        obj.push(data.len() as u8); // dlc
        obj.extend_from_slice(&raw_id.to_le_bytes());
        let mut payload = [0u8; 8];
        payload[..data.len()].copy_from_slice(data);
        obj.extend_from_slice(&payload);
        obj
    }

    /// Wrap raw object bytes into a LOG_CONTAINER (zlib or stored).
    pub fn container(objects: &[u8], zlib: bool) -> Vec<u8> {
        let payload = if zlib {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(objects).unwrap();
            enc.finish().unwrap()
        } else {
            objects.to_vec()
        };
        let object_size = 32 + payload.len();
        let mut obj = Vec::new();
        obj.extend_from_slice(OBJ_SIGNATURE);
        obj.extend_from_slice(&16u16.to_le_bytes()); // header size (base only)
        obj.extend_from_slice(&1u16.to_le_bytes());
        obj.extend_from_slice(&(object_size as u32).to_le_bytes());
        obj.extend_from_slice(&OBJ_TYPE_LOG_CONTAINER.to_le_bytes());
        obj.extend_from_slice(
            &if zlib {
                COMPRESSION_ZLIB
            } else {
                COMPRESSION_NONE
            }
            .to_le_bytes(),
        );
        obj.extend_from_slice(&[0u8; 6]);
        obj.extend_from_slice(&(objects.len() as u32).to_le_bytes());
        obj.extend_from_slice(&[0u8; 4]);
        obj.extend_from_slice(&payload);
        // BLF convention (python-can, vector_blf): pad with object_size % 4
        // bytes, not to true 4-byte alignment.
        obj.extend(std::iter::repeat_n(0u8, object_size % 4));
        obj
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn reads_frames_from_zlib_container() {
        let mut objects = Vec::new();
        objects.extend_from_slice(&can_message(1_000_000, 0x123, false, &[1, 2, 3, 4]));
        objects.extend_from_slice(&can_message(2_000_000, 0x1ABCDE, true, &[5, 6]));
        let mut file = file_header();
        file.extend_from_slice(&container(&objects, true));

        let mut reader = BlfReader::new(file.as_slice()).unwrap();
        let f1 = reader.next_frame().unwrap().unwrap();
        assert_eq!(f1.id, 0x123);
        assert!(!f1.is_extended);
        assert_eq!(f1.timestamp_ns, 1_000_000);
        assert_eq!(f1.data(), &[1, 2, 3, 4]);
        let f2 = reader.next_frame().unwrap().unwrap();
        assert_eq!(f2.id, 0x1ABCDE);
        assert!(f2.is_extended);
        assert_eq!(f2.data(), &[5, 6]);
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[test]
    fn object_split_across_containers() {
        let obj = can_message(5_000, 0x42, false, &[0xAA; 8]);
        let (a, b) = obj.split_at(20);
        let mut file = file_header();
        file.extend_from_slice(&container(a, true));
        file.extend_from_slice(&container(b, false));

        let mut reader = BlfReader::new(file.as_slice()).unwrap();
        let f = reader.next_frame().unwrap().unwrap();
        assert_eq!(f.id, 0x42);
        assert_eq!(f.data(), &[0xAA; 8]);
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[test]
    fn top_level_message_without_container() {
        let mut file = file_header();
        file.extend_from_slice(&can_message(7, 0x7FF, false, &[9]));
        let mut reader = BlfReader::new(file.as_slice()).unwrap();
        let f = reader.next_frame().unwrap().unwrap();
        assert_eq!(f.id, 0x7FF);
        assert!(reader.next_frame().unwrap().is_none());
    }

    #[test]
    fn header_start_time_epoch() {
        let file = file_header();
        let reader = BlfReader::new(file.as_slice()).unwrap();
        let t = reader.start_time.to_epoch_seconds();
        // 2024-01-02 03:04:05.678 UTC
        assert!((t - 1704164645.678).abs() < 1e-6, "{t}");
    }

    #[test]
    fn rejects_non_blf() {
        assert!(BlfReader::new(&b"not a blf file at all"[..]).is_err());
    }
}
