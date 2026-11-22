//! Anonymous pipe framing shared by the native parent and isolated HTML host.
use serde_json::Value;
use std::io::{BufRead, Write};
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;
pub fn read(reader: &mut impl BufRead) -> Result<Option<Value>, String> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|_| "pipe-read")?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err("pipe-truncated".into())
            };
        }
        let count = available
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(available.len());
        if bytes.len() + count > MAX_MESSAGE {
            return Err("pipe-message-too-large".into());
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            return serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| "pipe-invalid-json".into());
        }
    }
}
pub fn write(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "pipe-serialize")?;
    if bytes.len() + 1 > MAX_MESSAGE {
        return Err("pipe-message-too-large".into());
    }
    writer
        .write_all(&bytes)
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
        .map_err(|_| "pipe-write".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_and_abrupt_host_exit() {
        let value = serde_json::json!({"command":"checkpoint","text":"Локальный\nчерновик"});
        let mut bytes = vec![];
        write(&mut bytes, &value).unwrap();
        let mut r = std::io::Cursor::new(bytes);
        assert_eq!(read(&mut r).unwrap(), Some(value));
        assert_eq!(read(&mut r).unwrap(), None);
        assert!(read(&mut std::io::Cursor::new(b"{\"partial\":")).is_err());
        assert!(read(&mut std::io::Cursor::new(vec![b'x'; MAX_MESSAGE + 1])).is_err());
    }
}
