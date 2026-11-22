//! The existing M53RGB01 client representation; no packaging or conversion.
pub const RGB_HEADER: &[u8; 8] = b"M53RGB01";
pub fn rgb_dimensions(bytes: &[u8]) -> Result<(u16, u16), String> {
    if bytes.len() < 16 || &bytes[..8] != RGB_HEADER {
        return Err("invalid_rgb_header".into());
    }
    let width = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let height = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    if width == 0
        || height == 0
        || width > 16384
        || height > 16384
        || (width as u64 * height as u64 * 3 + 16) != bytes.len() as u64
    {
        return Err("invalid_rgb_size".into());
    }
    Ok((width as u16, height as u16))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn header(w: u32, h: u32) -> Vec<u8> {
        let mut value = RGB_HEADER.to_vec();
        value.extend(w.to_le_bytes());
        value.extend(h.to_le_bytes());
        value
    }
    #[test]
    fn preserves_exact_rgb_wire_format() {
        let mut bytes = header(2, 1);
        bytes.extend([0, 1, 255, 128, 64, 32]);
        let before = bytes.clone();
        assert_eq!(rgb_dimensions(&bytes).unwrap(), (2, 1));
        assert_eq!(bytes, before);
    }
    #[test]
    fn preserves_header_and_size_errors() {
        for bytes in [vec![], b"M53RGB01".to_vec(), b"WRONG12300000000".to_vec()] {
            assert_eq!(rgb_dimensions(&bytes).unwrap_err(), "invalid_rgb_header");
        }
        for bytes in [
            header(0, 1),
            header(1, 0),
            header(16385, 1),
            header(1, 16385),
            header(u32::MAX, u32::MAX),
            header(1, 1),
        ] {
            assert_eq!(rgb_dimensions(&bytes).unwrap_err(), "invalid_rgb_size");
        }
        let mut bytes = header(1, 1);
        bytes.extend([0, 1, 2, 3]);
        assert_eq!(rgb_dimensions(&bytes).unwrap_err(), "invalid_rgb_size");
    }
    #[test]
    fn keeps_inclusive_dimension_limit() {
        for (w, h) in [(16384, 1), (1, 16384)] {
            let mut bytes = header(w, h);
            bytes.resize((16 + w * h * 3) as usize, 42);
            assert_eq!(rgb_dimensions(&bytes).unwrap(), (w as u16, h as u16));
        }
    }
}
