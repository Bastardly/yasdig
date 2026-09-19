//! A tiny, dependency-free HTTP/1.1 server for a localhost single-user UI.
//! Not a general-purpose server: one request per connection (Connection: close),
//! bodies bounded by Content-Length against a per-route limit. Enough to serve a
//! static page, a JSON API and one binary upload, with zero external crates.

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

const MAX_BODY: usize = 64 * 1024; // request bodies are small JSON payloads

/// The one exception, and the reason the limit is per-route rather than global.
/// `POST /upload` carries a whole PNG as its body, which is megabytes — a 2048²
/// photo re-encoded by the browser lands around 6 MB and a 4096² one around 25.
/// Keeping every other route at 64 KB means a malformed `Content-Length` still
/// can't make the server allocate a gigabyte, since the vec below is sized
/// straight from the header.
const MAX_UPLOAD: usize = 32 * 1024 * 1024;

/// How large a body this path is allowed to send.
///
/// A free function, and an exact match rather than a prefix: `/uploads-of-mine`
/// must not inherit the large cap just for starting with the same letters. It is
/// separated out because it is the only decidable part of `read_request` — the
/// rest needs a socket — and a limit nobody can test is a limit that drifts.
fn body_limit(path: &str) -> usize {
    match path {
        "/upload" => MAX_UPLOAD,
        _ => MAX_BODY,
    }
}

pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

pub struct Connection {
    reader: BufReader<TcpStream>,
}

impl Connection {
    pub fn new(stream: TcpStream) -> Result<Self> {
        stream.set_nodelay(true).ok();
        Ok(Connection {
            reader: BufReader::new(stream),
        })
    }

    /// Parse the request line, headers, and (if present) a Content-Length body.
    /// Returns Ok(None) on an empty/closed connection.
    pub fn read_request(&mut self) -> Result<Option<Request>> {
        let mut request_line = String::new();
        let n = self
            .reader
            .read_line(&mut request_line)
            .context("reading request line")?;
        if n == 0 {
            return Ok(None);
        }

        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("/").to_string();
        if method.is_empty() {
            return Ok(None);
        }
        // Strip any query string for routing simplicity.
        let path = path.split('?').next().unwrap_or("/").to_string();

        // Headers.
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
        }

        // Answered rather than bailed. An `Err` out of here closes the socket
        // without writing anything at all (see `handle_conn`), so an oversized
        // upload used to surface in the browser as a dropped connection — which
        // is indistinguishable from the server having died. The body is not read
        // afterwards, but every response here is `Connection: close` anyway.
        if content_length > body_limit(&path) {
            self.respond(
                413,
                "application/json",
                format!(
                    "{{\"error\":\"request body too large ({content_length} bytes, limit {})\"}}",
                    body_limit(&path)
                )
                .as_bytes(),
            )?;
            return Ok(None);
        }

        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            self.reader
                .read_exact(&mut body)
                .context("reading request body")?;
        }

        Ok(Some(Request { method, path, body }))
    }

    pub fn respond(&mut self, status: u16, content_type: &str, body: &[u8]) -> Result<()> {
        self.respond_with(status, content_type, "", body)
    }

    /// `extra` is zero or more complete header lines, each ending in CRLF. It
    /// exists for the two static files, which are the only responses that want
    /// a caching header; everything else is generated per request.
    pub fn respond_with(
        &mut self,
        status: u16,
        content_type: &str,
        extra: &str,
        body: &[u8],
    ) -> Result<()> {
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            413 => "Payload Too Large",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            _ => "OK",
        };
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\n\
             Content-Type: {content_type}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             {extra}\r\n",
            body.len()
        );
        let stream = self.reader.get_mut();
        stream.write_all(header.as_bytes())?;
        stream.write_all(body)?;
        stream.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{body_limit, Connection, MAX_BODY, MAX_UPLOAD};

    /// The upload cap is 512× the JSON one, so which paths get it is worth
    /// stating. A prefix test would have handed it to anything beginning with
    /// the same eight characters, and this is the only place in the server where
    /// a client-supplied number decides an allocation.
    #[test]
    fn only_the_upload_route_may_send_a_large_body() {
        assert_eq!(body_limit("/upload"), MAX_UPLOAD);

        for small in [
            "/",
            "/generate",
            "/refine",
            "/img2img",
            "/uploads",
            "/upload/anything",
            "/x/upload",
            "",
        ] {
            assert_eq!(body_limit(small), MAX_BODY, "{small} should be capped small");
        }
    }

    /// `respond_with` splices caller-supplied header lines in ahead of the blank
    /// line that ends the header, which is a thing to get exactly right once: a
    /// missing CRLF there makes every response on that path unparseable, and the
    /// only route using it is the one that serves the page itself.
    #[test]
    fn extra_headers_land_before_the_blank_line() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut conn = Connection::new(stream).unwrap();
            // Read it before answering: closing a socket with unread bytes still
            // in it is an RST, which takes the response with it.
            conn.read_request().unwrap();
            conn.respond_with(200, "text/css; charset=utf-8", "Cache-Control: no-cache\r\n", b"a{}")
                .unwrap();
        });

        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client.write_all(b"GET /style.css HTTP/1.1\r\n\r\n").unwrap();
        let mut got = String::new();
        client.read_to_string(&mut got).unwrap();
        server.join().unwrap();

        assert_eq!(
            got,
            "HTTP/1.1 200 OK\r\n\
             Content-Type: text/css; charset=utf-8\r\n\
             Content-Length: 3\r\n\
             Connection: close\r\n\
             Cache-Control: no-cache\r\n\
             \r\na{}"
        );
    }
}
