//! A minimal, dependency-free PNG encoder for RGBA8 images — just enough to
//! write the `--dump` verification images. It emits a valid PNG using
//! *uncompressed* zlib "stored" DEFLATE blocks, so there is no compressor to
//! pull in; the files are large but perfectly valid and open in any viewer.
//!
//! Kept in `ra-client` (not `ra-formats`) because it is a presentation-side
//! convenience, not a game format. Client code may use whatever it likes here.

/// Encode an RGBA8 buffer (`width*height*4` bytes) as PNG bytes.
pub fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    assert_eq!(rgba.len(), (width as usize) * (height as usize) * 4);

    let mut out = Vec::new();
    out.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]); // signature

    // IHDR: width, height, bit depth 8, color type 6 (RGBA), no interlace.
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    write_chunk(&mut out, b"IHDR", &ihdr);

    // IDAT: filtered scanlines (filter byte 0 per row) wrapped in a zlib stream.
    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    let stride = width as usize * 4;
    for y in 0..height as usize {
        raw.push(0); // filter type 0 (None)
        raw.extend_from_slice(&rgba[y * stride..(y + 1) * stride]);
    }
    let zlib = zlib_stored(&raw);
    write_chunk(&mut out, b"IDAT", &zlib);

    write_chunk(&mut out, b"IEND", &[]);
    out
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// Wrap `data` in a zlib stream using only uncompressed DEFLATE "stored" blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x78); // CMF: 32K window, deflate
    out.push(0x01); // FLG: no dict, fastest (check bits make 0x7801 % 31 == 0)

    // DEFLATE stored blocks: each holds up to 65535 bytes.
    let mut chunks = data.chunks(0xFFFF).peekable();
    if data.is_empty() {
        // One empty final block.
        out.push(0x01);
        out.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
    }
    while let Some(chunk) = chunks.next() {
        let final_block = chunks.peek().is_none();
        out.push(if final_block { 1 } else { 0 }); // BFINAL, BTYPE=00
        let len = chunk.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
    }

    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

struct Crc32 {
    crc: u32,
}

impl Crc32 {
    fn new() -> Crc32 {
        Crc32 { crc: 0xFFFF_FFFF }
    }
    fn update(&mut self, data: &[u8]) {
        for &byte in data {
            let mut c = (self.crc ^ byte as u32) & 0xFF;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            self.crc = c ^ (self.crc >> 8);
        }
    }
    fn finish(self) -> u32 {
        self.crc ^ 0xFFFF_FFFF
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_valid_signature_and_chunks() {
        let png = encode_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(
            &png[0..8],
            &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
        );
        // IHDR chunk type appears right after the 8-byte sig + 4-byte length.
        assert_eq!(&png[12..16], b"IHDR");
        // ends with IEND + crc
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn adler_and_crc_known_values() {
        assert_eq!(adler32(b""), 1);
        // zlib adler of "abc" is 0x024d0127
        assert_eq!(adler32(b"abc"), 0x024d_0127);
        let mut c = Crc32::new();
        c.update(b"123456789");
        assert_eq!(c.finish(), 0xCBF4_3926); // standard CRC-32 check value
    }
}
