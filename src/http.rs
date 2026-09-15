//! A tiny, dependency-free HTTP/1.1 server for a localhost single-user UI.
//! Not a general-purpose server: one request per connection (Connection: close),
//! bodies bounded by Content-Length. Enough to serve a static page and a JSON
//! API, with zero external crates.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;

const MAX_BODY: usize = 64 * 1024; // request bodies are small JSON payloads

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

        if content_length > MAX_BODY {
            bail!("request body too large ({content_length} bytes)");
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
        let reason = match status {
            200 => "OK",
            400 => "Bad Request",
            404 => "Not Found",
            500 => "Internal Server Error",
            503 => "Service Unavailable",
            _ => "OK",
        };
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\n\
             Content-Type: {content_type}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             \r\n",
            body.len()
        );
        let stream = self.reader.get_mut();
        stream.write_all(header.as_bytes())?;
        stream.write_all(body)?;
        stream.flush()?;
        Ok(())
    }
}
