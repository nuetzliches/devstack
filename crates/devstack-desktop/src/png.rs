//! The smallest PNG writer: 8-bit RGBA, one unfiltered IDAT. For a
//! program's icon (ADR-0046), which the system hands over as pixels.

use flate2::{Compression, Crc, write::ZlibEncoder};
use std::io::Write;

/// `rgba`, `width` by `height` pixels, top row first, as a PNG file.
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let row = width as usize * 4;
    debug_assert_eq!(rgba.len(), row * height as usize);
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in rgba.chunks_exact(row) {
        // Filter type 0: the row as it is.
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(&raw).expect("writing to memory");
    let idat = z.finish().expect("writing to memory");

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // 8 bits per sample, colour type 6 (RGBA), deflate, no filter set, no
    // interlace.
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1x1 transparent pixel; the CRC of an empty IEND is fixed by the
    /// format.
    #[test]
    fn a_pixel() {
        let png = encode(1, 1, &[0, 0, 0, 0]);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(&png[12..16], b"IHDR");
        assert!(png.ends_with(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]));
    }
}
