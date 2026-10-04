//! A minimal PNG encoder (8-bit RGB, stored deflate blocks) for synthetic `cid:` images.

/// Encodes `pixels` (row-major RGB, three bytes per pixel) as a PNG file.
pub fn encode_rgb(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let stride = width as usize * 3;
    assert_eq!(pixels.len(), stride * height as usize, "pixel buffer size");
    let mut raw = Vec::with_capacity((stride + 1) * height as usize);
    for row in pixels.chunks_exact(stride.max(1)) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib_stored(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// A synthetic picture: a diagonal gradient between two colours with stripes and a block,
/// so no two rows look alike and a snapshot of it is never uniform.
pub fn picture(width: u32, height: u32, from: [u8; 3], to: [u8; 3]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 3);
    let span = (width + height).max(1) as f32;
    for y in 0..height {
        for x in 0..width {
            let t = (x + y) as f32 / span;
            let stripe = (x / 12 + y / 12) % 7 == 0;
            let block = x > width / 3 && x < width / 2 && y > height / 3 && y < height * 2 / 3;
            for channel in 0..3 {
                let base = f32::from(from[channel]) * (1.0 - t) + f32::from(to[channel]) * t;
                let value = if block {
                    255.0 - base
                } else if stripe {
                    base * 0.8
                } else {
                    base
                };
                pixels.push(value as u8);
            }
        }
    }
    encode_rgb(width, height, &pixels)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    let mut blocks = data.chunks(usize::from(u16::MAX)).peekable();
    while let Some(block) = blocks.next() {
        out.push(u8::from(blocks.peek().is_none()));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65_521;
        b %= 65_521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_match_known_values() {
        assert_eq!(crc32(b"IEND"), 0xae42_6082);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn png_has_signature_and_chunks() {
        let png = picture(70_000 / 300, 300, [0, 0, 0], [255, 255, 255]);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(&png[12..16], b"IHDR");
        assert!(png.ends_with(&[0xae, 0x42, 0x60, 0x82]));
    }
}
