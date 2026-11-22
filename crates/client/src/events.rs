//! Bounded SSE invalidation parser. No clinical data or replay cursor is trusted.
#[derive(Default)]
pub struct Parser {
    buffer: Vec<u8>,
    event: String,
}
#[derive(Debug, PartialEq)]
pub enum Signal {
    Refresh,
    TemplatesUpdated,
    AuthRequired,
}
impl Parser {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Signal>, ()> {
        if self.buffer.len() + bytes.len() > 65536 {
            return Err(());
        }
        self.buffer.extend_from_slice(bytes);
        let mut signals = Vec::new();
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.buffer.drain(..=end).collect();
            let line = std::str::from_utf8(&line)
                .map_err(|_| ())?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                match self.event.as_str() {
                    "refresh" => signals.push(Signal::Refresh),
                    "templates-updated" => signals.push(Signal::TemplatesUpdated),
                    "auth-required" => signals.push(Signal::AuthRequired),
                    _ => {}
                }
                self.event.clear();
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = value.trim_start().to_owned();
            }
        }
        Ok(signals)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_frames_keepalive_and_auth() {
        let mut p = Parser::default();
        assert!(p.push(b": keepalive\r\nevent: ref").unwrap().is_empty());
        assert_eq!(
            p.push(b"resh\r\ndata: {}\r\n\r\nevent: auth-required\ndata: {}\n\n")
                .unwrap(),
            vec![Signal::Refresh, Signal::AuthRequired]
        );
    }
    #[test]
    fn bounded_and_unknown_events() {
        let mut p = Parser::default();
        assert!(p.push(b"event: unknown\n\n").unwrap().is_empty());
        assert!(p.push(&vec![b'x'; 65537]).is_err());
    }
    #[test]
    fn stationery_changes_invalidate_only_template_metadata() {
        let mut p=Parser::default();
        assert!(p.push(b"event: templates-up").unwrap().is_empty());
        assert_eq!(p.push(b"dated\ndata: {}\n\n").unwrap(),vec![Signal::TemplatesUpdated]);
    }
}
