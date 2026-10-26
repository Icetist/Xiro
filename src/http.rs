//! Just enough HTTP/1.1 for the pairing page, the static client and the WebSocket
//! upgrade: GET requests without bodies, a whitelist of embedded files, strict headers.

use std::collections::HashMap;

/// Larger request heads are refused: no legitimate client sends them.
pub const MAX_HEAD_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// Lower-cased names.
    pub headers: HashMap<String, String>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    pub fn wants_websocket(&self) -> bool {
        self.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"))
            && self.header("connection").is_some_and(|c| c.to_ascii_lowercase().split(',').any(|p| p.trim() == "upgrade"))
    }

    pub fn keep_alive(&self) -> bool {
        !self.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"))
    }
}

/// Index just past the blank line ending the request head, if it has arrived.
pub fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn hex(digit: u8) -> Option<u8> {
    (digit as char).to_digit(16).map(|d| d as u8)
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi << 4 | lo);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

/// Parses a request head; None for anything malformed.
pub fn parse(head: &[u8]) -> Option<Request> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut parts = lines.next()?.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !version.starts_with("HTTP/1.") || !target.starts_with('/') {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut headers = HashMap::new();
    for line in lines.take_while(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':')?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    Some(Request { method: method.to_owned(), path: path.to_owned(), query: parse_query(query), headers })
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: Vec<u8>,
}

fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    }
}

impl Response {
    pub fn new(status: u16, body: Vec<u8>) -> Self {
        Self { status, headers: Vec::new(), body }
    }

    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub fn to_bytes(&self, keep_alive: bool) -> Vec<u8> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status));
        for (name, value) in &self.headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        if self.status != 101 {
            head.push_str(&format!("Content-Length: {}\r\n", self.body.len()));
            head.push_str(if keep_alive { "Connection: keep-alive\r\n" } else { "Connection: close\r\n" });
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

/// Same-origin check: the page that opens the socket must be our own page.
pub fn is_same_origin(req: &Request) -> bool {
    let (Some(origin), Some(host)) = (req.header("origin"), req.header("host")) else { return false };
    let Some(rest) = origin.strip_prefix("https://").or_else(|| origin.strip_prefix("http://")) else { return false };
    rest.split('/').next() == Some(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UPGRADE: &[u8] = b"GET /ws?token=a%2Bb&x HTTP/1.1\r\nHost: 192.168.1.5:8443\r\nUpgrade: websocket\r\nConnection: keep-alive, Upgrade\r\nOrigin: https://192.168.1.5:8443\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";

    #[test]
    fn parses_requests_with_query_and_headers() {
        let req = parse(UPGRADE).unwrap();
        assert_eq!((req.method.as_str(), req.path.as_str()), ("GET", "/ws"));
        assert_eq!(req.query.get("token").map(String::as_str), Some("a+b"));
        assert_eq!(req.query.get("x").map(String::as_str), Some(""));
        assert_eq!(req.header("host"), Some("192.168.1.5:8443"));
        assert!(req.wants_websocket());
        assert!(is_same_origin(&req));
        assert!(req.keep_alive());
        assert_eq!(head_end(UPGRADE), Some(UPGRADE.len()));
    }

    #[test]
    fn rejects_malformed_heads() {
        assert!(parse(b"GET\r\n\r\n").is_none());
        assert!(parse(b"GET http://x/ HTTP/1.1\r\n\r\n").is_none());
        assert!(parse(b"GET / SPDY\r\n\r\n").is_none());
        assert!(parse(b"GET / HTTP/1.1\r\nbroken header\r\n\r\n").is_none());
        assert!(parse(&[0xff, 0xfe]).is_none());
        assert_eq!(head_end(b"GET / HTTP/1.1\r\n"), None);
    }

    #[test]
    fn cross_origin_sockets_are_refused() {
        let mut req = parse(UPGRADE).unwrap();
        req.headers.insert("origin".into(), "https://evil.example".into());
        assert!(!is_same_origin(&req));
        req.headers.remove("origin");
        assert!(!is_same_origin(&req));
        req.headers.insert("origin".into(), "null".into());
        assert!(!is_same_origin(&req));
    }

    #[test]
    fn percent_decoding_handles_bad_escapes() {
        assert_eq!(percent_decode("a%20b%zz%"), "a b%zz%");
        assert_eq!(percent_decode("%41"), "A");
    }

    #[test]
    fn responses_carry_length_and_connection() {
        let bytes = Response::new(404, b"Not found".to_vec()).header("X-Test", "1").to_bytes(false);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 404 Not Found\r\nX-Test: 1\r\nContent-Length: 9\r\nConnection: close\r\n\r\nNot found"));
        let upgrade = String::from_utf8(Response::new(101, Vec::new()).to_bytes(true)).unwrap();
        assert!(!upgrade.contains("Content-Length"));
    }
}
