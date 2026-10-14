//! A tiny, dependency-free HTTP/1.1 server for a localhost single-user UI.
//! Not a general-purpose server: one request per connection (Connection: close),
//! bodies bounded by Content-Length against a per-route limit. Enough to serve a
//! static page, a JSON API and one binary upload, with zero external crates.

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::Duration;

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
/// A request line or header line longer than this is refused. The longest
/// legitimate one is a browser's `Accept` or `User-Agent`, well under 1 KB; an
/// unbounded `read_line` would let a client that never sends a newline grow the
/// line until the process runs out of memory.
const MAX_LINE: u64 = 8 * 1024;
const MAX_HEADERS: usize = 100;

/// How long a client may take to *send* its request. Only the request: once
/// the body is in, the timeout is lifted, because the answer to a Hi-res or a
/// video can be minutes away and that wait is on the worker's reply channel,
/// not on this socket. Without it, a client that opens a connection and sends
/// nothing holds a thread forever.
#[cfg(not(test))]
const READ_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(test)]
const READ_TIMEOUT: Duration = Duration::from_millis(200);

/// An over-limit body is read and thrown away up to this much, so the 413 is
/// what the browser sees rather than the reset a close with unread bytes sends.
/// Past it the reset is the lesser evil — the client is not a browser posting
/// a picture.
const MAX_DRAIN: u64 = 256 * 1024 * 1024;

fn body_limit(path: &str) -> usize {
    match path {
        "/upload" => MAX_UPLOAD,
        // A mask at the image's size, base64 in JSON. Mostly one colour, so
        // small — but a busy one at 3072² is well past `MAX_BODY`.
        "/redraw" => MAX_UPLOAD,
        // A brush tool's whole mask, the same shape as Redraw's.
        "/brush" => MAX_UPLOAD,
        // A whole PNG, read for its text chunks.
        "/png-settings" => MAX_UPLOAD,
        _ => MAX_BODY,
    }
}

pub struct Request {
    pub method: String,
    pub path: String,
    /// The `Origin` and `Host` headers, for [`allowed`]. Absent is `None`.
    pub origin: Option<String>,
    pub host: Option<String>,
    pub body: Vec<u8>,
}

/// Whether a request may reach a route: the guard against other websites.
///
/// The server has no login, so *any page open in the same browser* could
/// otherwise drive it. Two attacks, two rules:
///
/// - **Cross-site POST.** A `text/plain` POST needs no CORS preflight, so a
///   page anywhere could `fetch("http://127.0.0.1:8080/shutdown", …)` and have
///   it run. A browser always sends `Origin` on a cross-origin POST, so a
///   non-GET whose `Origin` isn't this server's own address is refused. No
///   `Origin` at all is a non-browser client (curl, the takeover in
///   `startup::close`, and older builds of that), and is let through.
/// - **DNS rebinding.** A hostile name re-pointed at 127.0.0.1 makes the
///   browser treat this server as same-origin with the attacker's page, which
///   could then *read* `/outputs`. The request still carries the hostile name
///   in `Host`, so a `Host` that isn't a loopback name (or the configured host)
///   is refused. The port is not compared: the takeover has always sent
///   `Host: localhost` with none, and the name is what an attacker can't fake.
///
/// `bound` is `startup.toml`'s host. Bound to every interface (`0.0.0.0`,
/// `::`), the server is reachable by whatever name the LAN uses for this
/// machine, so `Host` is not checked — the `Origin` rule still is.
pub fn allowed(method: &str, origin: Option<&str>, host: Option<&str>, bound: &str) -> bool {
    let any_host = matches!(bound, "0.0.0.0" | "::" | "[::]");
    if let Some(h) = host {
        if !any_host && !is_local_name(hostname(h), bound) {
            return false;
        }
    }
    if method == "GET" {
        return true;
    }
    match origin {
        None => true,
        // The page's own requests: `Origin` is `http://` plus exactly the
        // `Host` the browser sent with it.
        Some(o) => host.is_some_and(|h| o.eq_ignore_ascii_case(&format!("http://{h}"))),
    }
}

/// `Host` without its port. `[::1]:8080` keeps its brackets.
fn hostname(host: &str) -> &str {
    if host.starts_with('[') {
        return host.split_inclusive(']').next().unwrap_or(host);
    }
    host.split(':').next().unwrap_or(host)
}

fn is_local_name(name: &str, bound: &str) -> bool {
    ["localhost", "127.0.0.1", "[::1]", bound]
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
}

pub struct Connection {
    reader: BufReader<TcpStream>,
}

impl Connection {
    pub fn new(stream: TcpStream) -> Result<Self> {
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(READ_TIMEOUT)).ok();
        Ok(Connection {
            reader: BufReader::new(stream),
        })
    }

    /// One line of the request head, `None` at end of stream. Bounded by
    /// [`MAX_LINE`]: a line that fills it without ending is refused.
    fn line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        let n = (&mut self.reader).take(MAX_LINE).read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        if n as u64 == MAX_LINE && !line.ends_with('\n') {
            anyhow::bail!("request header line longer than {MAX_LINE} bytes");
        }
        Ok(Some(line))
    }

    /// Parse the request line, headers, and (if present) a Content-Length body.
    /// Returns Ok(None) on an empty/closed connection.
    pub fn read_request(&mut self) -> Result<Option<Request>> {
        let Some(request_line) = self.line().context("reading request line")? else {
            return Ok(None);
        };

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
        let mut origin = None;
        let mut host = None;
        let mut count = 0;
        while let Some(line) = self.line()? {
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            count += 1;
            if count > MAX_HEADERS {
                anyhow::bail!("more than {MAX_HEADERS} request headers");
            }
            if let Some((k, v)) = line.split_once(':') {
                let (k, v) = (k.trim(), v.trim());
                if k.eq_ignore_ascii_case("content-length") {
                    content_length = v.parse().unwrap_or(0);
                } else if k.eq_ignore_ascii_case("origin") {
                    origin = Some(v.to_string());
                } else if k.eq_ignore_ascii_case("host") {
                    host = Some(v.to_string());
                }
            }
        }

        // Answered rather than bailed. An `Err` out of here closes the socket
        // without writing anything at all (see `handle_conn`), so an oversized
        // upload used to surface in the browser as a dropped connection — which
        // is indistinguishable from the server having died.
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
            // And then the body is read and dropped. Closing with it unread
            // sends a reset, which a browser still sending the upload reports
            // as a dropped connection — taking the 413 with it.
            let stream = self.reader.get_ref();
            stream.shutdown(Shutdown::Write).ok();
            let _ = std::io::copy(
                &mut (&mut self.reader).take((content_length as u64).min(MAX_DRAIN)),
                &mut std::io::sink(),
            );
            return Ok(None);
        }

        // Read as it arrives rather than allocated up front from the header:
        // a client that declares 32 MB and then sends nothing holds no memory.
        let mut body = Vec::with_capacity(content_length.min(1 << 20));
        (&mut self.reader)
            .take(content_length as u64)
            .read_to_end(&mut body)
            .context("reading request body")?;
        if body.len() != content_length {
            anyhow::bail!("request body ended after {} of {content_length} bytes", body.len());
        }
        // The request is in; what follows may be a minutes-long render.
        self.reader.get_ref().set_read_timeout(None).ok();

        Ok(Some(Request { method, path, origin, host, body }))
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
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
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
    use super::{allowed, body_limit, Connection, MAX_BODY, MAX_UPLOAD, READ_TIMEOUT};
    use std::io::{Read, Write};

    /// Serve one request on a fresh port: `answer` gets the connection after
    /// `read_request`, and the client's raw bytes come back as a string.
    fn exchange(sent: &[u8], answer: impl FnOnce(&mut Connection) + Send + 'static) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut conn = Connection::new(stream).unwrap();
            answer(&mut conn);
        });
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        client.write_all(sent).unwrap();
        let mut got = String::new();
        // A refused request may end in a reset; what arrived before it counts.
        client.read_to_string(&mut got).ok();
        server.join().unwrap();
        got
    }

    /// The table behind the cross-site guard. The page's own requests, curl and
    /// the takeover (`Host: localhost`, no port, no `Origin`) pass; another
    /// site's POST and a rebound hostname don't.
    #[test]
    fn only_this_servers_own_page_may_post() {
        let lo = "127.0.0.1";
        // The page.
        assert!(allowed("POST", Some("http://127.0.0.1:8080"), Some("127.0.0.1:8080"), lo));
        assert!(allowed("POST", Some("http://localhost:8081"), Some("localhost:8081"), lo));
        assert!(allowed("POST", Some("http://[::1]:8080"), Some("[::1]:8080"), lo));
        // curl, and `startup::close` in this build and every older one.
        assert!(allowed("POST", None, Some("localhost"), lo));
        assert!(allowed("POST", None, None, lo));
        // Another website.
        assert!(!allowed("POST", Some("https://evil.example"), Some("127.0.0.1:8080"), lo));
        assert!(!allowed("POST", Some("null"), Some("127.0.0.1:8080"), lo));
        // Same host, another port: another origin.
        assert!(!allowed("POST", Some("http://127.0.0.1:9999"), Some("127.0.0.1:8080"), lo));
        // DNS rebinding, read or write.
        assert!(!allowed("GET", None, Some("evil.example:8080"), lo));
        assert!(!allowed("POST", Some("http://evil.example:8080"), Some("evil.example:8080"), lo));
        // A cross-site GET can't read the answer without CORS, so it passes.
        assert!(allowed("GET", Some("https://evil.example"), Some("127.0.0.1:8080"), lo));
        // Open to the LAN: any name for this machine, still same-origin only.
        let any = "0.0.0.0";
        assert!(allowed("POST", Some("http://studio.lan:8080"), Some("studio.lan:8080"), any));
        assert!(!allowed("POST", Some("https://evil.example"), Some("studio.lan:8080"), any));
        // A configured name is local too.
        assert!(allowed("GET", None, Some("studio.lan:8080"), "studio.lan"));
    }

    /// The 413 has to reach the browser. Closing with the body unread sends a
    /// reset, which takes the response with it — so the body is drained.
    #[test]
    fn an_oversized_body_still_gets_its_413() {
        let len = MAX_BODY + 1;
        let mut sent = format!("POST /generate HTTP/1.1\r\nContent-Length: {len}\r\n\r\n").into_bytes();
        sent.extend(std::iter::repeat_n(b'x', len));
        let got = exchange(&sent, |conn| assert!(conn.read_request().unwrap().is_none()));
        assert!(got.starts_with("HTTP/1.1 413 "), "{got}");
    }

    /// The read timeout is for the request only: an answer that takes longer
    /// than it (a render) still arrives.
    #[test]
    fn a_slow_answer_outlives_the_read_timeout() {
        let got = exchange(b"GET /jobs HTTP/1.1\r\n\r\n", |conn| {
            conn.read_request().unwrap().unwrap();
            std::thread::sleep(READ_TIMEOUT * 3);
            conn.respond(200, "text/plain", b"done").unwrap();
        });
        assert!(got.ends_with("done"), "{got}");
    }

    /// A header line that never ends is refused rather than grown.
    #[test]
    fn an_endless_header_line_is_refused() {
        let mut sent = b"GET / HTTP/1.1\r\nX: ".to_vec();
        sent.extend(std::iter::repeat_n(b'a', 20_000));
        sent.extend(b"\r\n\r\n");
        exchange(&sent, |conn| assert!(conn.read_request().is_err()));
    }

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
