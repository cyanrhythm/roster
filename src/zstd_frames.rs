//! Zstandard frame primitives for DSH session logs.
//!
//! DSH stores a session as a concatenated-frame container: the first frame is
//! exactly one header line, and every following frame carries durable event
//! batches. The reader validates that the first frame contains one line only,
//! so writers must preserve that invariant.

use std::io::Read;

use anyhow::{bail, Result};

const ZSTD_MAGIC: u32 = 0xFD2F_B528;

#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub start: usize,
    pub end: usize,
}

/// Locate complete Zstandard frames without decompressing their blocks.
pub fn scan_frames(buf: &[u8]) -> Result<Vec<Frame>> {
    let mut frames = Vec::new();
    let mut off = 0usize;
    while off < buf.len() {
        let start = off;
        if buf.len() - off < 4 {
            bail!("torn frame header at byte {off}");
        }
        let magic = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        if magic != ZSTD_MAGIC {
            bail!("invalid Zstandard frame magic at byte {off}");
        }
        off += 4;
        if off == buf.len() {
            bail!("torn frame at byte {start}");
        }
        let descriptor = buf[off];
        off += 1;
        if descriptor & 0x18 != 0 {
            bail!("reserved frame-header bit at byte {}", off - 1);
        }
        let content_size_flag = descriptor >> 6;
        let single_segment = descriptor & 0x20 != 0;
        let checksum = descriptor & 0x04 != 0;
        let dict_flag = descriptor & 0x03;
        let dict_bytes = if dict_flag == 3 { 4 } else { dict_flag as usize };
        let content_size_bytes = if content_size_flag == 0 {
            if single_segment {
                1
            } else {
                0
            }
        } else {
            1usize << content_size_flag
        };
        let remaining = if single_segment { 0 } else { 1 } + dict_bytes + content_size_bytes;
        if buf.len() - off < remaining {
            bail!("torn frame header at byte {start}");
        }
        off += remaining;
        loop {
            if buf.len() - off < 3 {
                bail!("torn block header at byte {start}");
            }
            let block_header = (buf[off] as u32) | ((buf[off + 1] as u32) << 8) | ((buf[off + 2] as u32) << 16);
            off += 3;
            let last_block = block_header & 1 != 0;
            let block_type = (block_header >> 1) & 3;
            let block_size = (block_header >> 3) as usize;
            if block_type == 3 {
                bail!("reserved block type at byte {}", off - 3);
            }
            let payload = if block_type == 1 { 1 } else { block_size };
            if buf.len() - off < payload {
                bail!("torn block at byte {start}");
            }
            off += payload;
            if last_block {
                break;
            }
        }
        if checksum {
            if buf.len() - off < 4 {
                bail!("torn checksum at byte {start}");
            }
            off += 4;
        }
        frames.push(Frame { start, end: off });
    }
    Ok(frames)
}

/// Decode exactly one complete frame.
pub fn decompress_frame(buf: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(buf)?;
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Compress one independently decodable frame.
pub fn compress_frame(data: &[u8]) -> Result<Vec<u8>> {
    Ok(zstd::stream::encode_all(data, 3)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_decodes_concatenated_frames() {
        let header = b"{\"type\":\"session\"}\n";
        let body = b"{\"seq\":0}\n{\"seq\":1}\n";
        let mut container = compress_frame(header).unwrap();
        container.extend_from_slice(&compress_frame(body).unwrap());

        let frames = scan_frames(&container).unwrap();
        assert_eq!(frames.len(), 2);
        let first = decompress_frame(&container[frames[0].start..frames[0].end]).unwrap();
        assert_eq!(first, header);
        let second = decompress_frame(&container[frames[1].start..frames[1].end]).unwrap();
        assert_eq!(second, body);
    }
}
