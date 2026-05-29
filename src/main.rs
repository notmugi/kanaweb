//! kanaweb-server
//!
//! A tiny zero-dependency HTTP server that serves the Flashcards.html app
//! and accepts PUT requests to save vocab.json next to it.
//!
//! Usage:
//!   kanaweb-server [--host HOST] [--port PORT] [--dir DIR]
//!
//! Defaults: host 127.0.0.1, port 8080, dir = current working directory.
//!
//! What it supports (everything Flashcards.html needs):
//!   - GET  /                  -> serves Flashcards.html
//!   - GET  /<file>            -> serves <file> from --dir (static)
//!   - HEAD /<file>            -> same as GET, no body
//!   - PUT  /vocab.json        -> writes the request body to vocab.json
//!   - PUT  /.vocab.json       -> writes the request body to .vocab.json
//!
//! Safety:
//!   - Path traversal is blocked: no "..", no absolute paths, no leading "/".
//!   - PUT is allow-listed to exactly the two vocab filenames the app uses.
//!   - Writes are atomic (write to temp file in same dir, then rename).
//!   - Request body size is capped (default 8 MiB) to avoid DoS via huge PUTs.
//!
//! The server is intentionally small and threaded one-connection-per-thread.
//! Good enough for a single user (or a handful) on a LAN.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// ───────────────────────────────────────────────────────────────────────────
// Config
// ───────────────────────────────────────────────────────────────────────────

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_INDEX: &str = "Flashcards.html";
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024; // 8 MiB
const MAX_HEADER_BYTES: usize = 16 * 1024;      // 16 KiB
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// Files Flashcards.html is allowed to PUT.
/// Matches `VOCAB_FILES` in Flashcards.html.
const PUT_ALLOWLIST: &[&str] = &["vocab.json", ".vocab.json"];

struct Config {
    host: String,
    port: u16,
    dir: PathBuf,
    index: String,
}

impl Config {
    fn parse_args() -> Result<Self, String> {
        let mut host = DEFAULT_HOST.to_string();
        let mut port = DEFAULT_PORT;
        let mut dir = std::env::current_dir()
            .map_err(|e| format!("cannot read current dir: {e}"))?;
        let index = DEFAULT_INDEX.to_string();

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                "--host" => {
                    host = args.next().ok_or("--host needs a value")?.to_string();
                }
                "-p" | "--port" => {
                    let v = args.next().ok_or("--port needs a value")?;
                    port = v.parse().map_err(|_| format!("invalid port: {v}"))?;
                }
                "-d" | "--dir" => {
                    let v = args.next().ok_or("--dir needs a value")?;
                    dir = PathBuf::from(v);
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }

        let dir = fs::canonicalize(&dir)
            .map_err(|e| format!("cannot resolve --dir {}: {e}", dir.display()))?;
        if !dir.is_dir() {
            return Err(format!("--dir is not a directory: {}", dir.display()));
        }

        Ok(Config { host, port, dir, index })
    }
}

fn print_help() {
    println!(
        "kanaweb-server — tiny static+PUT server for the Flashcards app

USAGE:
    kanaweb-server [--host HOST] [--port PORT] [--dir DIR]

OPTIONS:
    --host HOST    Bind address (default: {DEFAULT_HOST})
    -p, --port P   Port (default: {DEFAULT_PORT})
    -d, --dir DIR  Directory to serve (default: current dir)
    -h, --help     Show this help

The server serves files from DIR and accepts PUT requests to save
vocab.json (or .vocab.json) back to DIR.

Visit http://HOST:PORT/ to load {DEFAULT_INDEX}.
"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Entry point
// ───────────────────────────────────────────────────────────────────────────

fn main() {
    let cfg = match Config::parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("try --help");
            std::process::exit(2);
        }
    };

    let addr = format!("{}:{}", cfg.host, cfg.port);
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("failed to bind {addr}: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("kanaweb-server listening on http://{addr}");
    eprintln!("serving directory: {}", cfg.dir.display());
    eprintln!("open http://{addr}/ to load {}", cfg.index);

    let cfg = Arc::new(cfg);
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let cfg = Arc::clone(&cfg);
                thread::spawn(move || {
                    if let Err(e) = handle_connection(s, cfg) {
                        // Connection-level errors are usually just clients
                        // hanging up. Log at debug-ish verbosity.
                        eprintln!("conn error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Request handling
// ───────────────────────────────────────────────────────────────────────────

fn handle_connection(stream: TcpStream, cfg: Arc<Config>) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

    let peer = stream.peer_addr().ok();
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;

    let req = match read_request(&mut reader) {
        Ok(r) => r,
        Err(e) => {
            // Malformed request; respond 400 and bail.
            let _ = write_response(&mut writer, 400, "text/plain; charset=utf-8", b"bad request", false);
            return Err(e);
        }
    };

    let method = req.method.as_str();
    let path = req.path.as_str();
    eprintln!(
        "{} {} {} {}",
        peer.map(|p| p.to_string()).unwrap_or_else(|| "?".into()),
        method,
        path,
        req.headers.get("content-length").map(|s| s.as_str()).unwrap_or("-")
    );

    match method {
        "GET" | "HEAD" => handle_get(&cfg, &req, &mut reader, &mut writer, method == "HEAD"),
        "PUT" => handle_put(&cfg, &req, &mut reader, &mut writer),
        "OPTIONS" => {
            // Same-origin app doesn't need CORS, but answering OPTIONS sanely
            // makes debugging easier.
            let mut extra = Vec::new();
            extra.push(("Allow", "GET, HEAD, PUT, OPTIONS"));
            write_response_ex(&mut writer, 204, "", b"", false, &extra)
        }
        _ => {
            let body = b"method not allowed";
            let extra = [("Allow", "GET, HEAD, PUT, OPTIONS")];
            write_response_ex(&mut writer, 405, "text/plain; charset=utf-8", body, false, &extra)
        }
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

fn read_request<R: BufRead>(reader: &mut R) -> io::Result<Request> {
    // Read request line.
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "empty request"));
    }
    let line = line.trim_end_matches(['\r', '\n']);
    let mut parts = line.splitn(3, ' ');
    let method = parts.next().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no method"))?.to_string();
    let raw_path = parts.next().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no path"))?.to_string();
    let _version = parts.next().unwrap_or("HTTP/1.0");

    // Strip query string — the app doesn't use any, and we don't either.
    let path = raw_path.split('?').next().unwrap_or("/").to_string();

    // Read headers.
    let mut headers = HashMap::new();
    let mut total = 0usize;
    loop {
        let mut buf = String::new();
        let read = reader.read_line(&mut buf)?;
        if read == 0 {
            break;
        }
        total += read;
        if total > MAX_HEADER_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "headers too large"));
        }
        let trimmed = buf.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(idx) = trimmed.find(':') {
            let name = trimmed[..idx].trim().to_ascii_lowercase();
            let value = trimmed[idx + 1..].trim().to_string();
            headers.insert(name, value);
        }
    }

    Ok(Request { method, path, headers })
}

// ───────────────────────────────────────────────────────────────────────────
// GET / HEAD
// ───────────────────────────────────────────────────────────────────────────

fn handle_get<R: BufRead, W: Write>(
    cfg: &Config,
    req: &Request,
    _reader: &mut R,
    writer: &mut W,
    head_only: bool,
) -> io::Result<()> {
    // Resolve "/" -> index file.
    let rel = if req.path == "/" {
        cfg.index.clone()
    } else {
        // Strip leading "/" only; preserves a path like "static/foo.js".
        req.path.trim_start_matches('/').to_string()
    };

    let safe = match safe_join(&cfg.dir, &rel) {
        Some(p) => p,
        None => {
            return write_response(writer, 403, "text/plain; charset=utf-8", b"forbidden", head_only);
        }
    };

    if !safe.is_file() {
        return write_response(writer, 404, "text/plain; charset=utf-8", b"not found", head_only);
    }

    let body = match fs::read(&safe) {
        Ok(b) => b,
        Err(_) => return write_response(writer, 500, "text/plain; charset=utf-8", b"read error", head_only),
    };
    let mime = mime_for(&safe);
    write_response(writer, 200, mime, &body, head_only)
}

// ───────────────────────────────────────────────────────────────────────────
// PUT
// ───────────────────────────────────────────────────────────────────────────

fn handle_put<R: BufRead, W: Write>(
    cfg: &Config,
    req: &Request,
    reader: &mut R,
    writer: &mut W,
) -> io::Result<()> {
    let rel = req.path.trim_start_matches('/').to_string();

    // Only allow the specific vocab filenames the app writes.
    // This is the security gate that keeps PUT from becoming arbitrary
    // file-upload-anywhere.
    if !PUT_ALLOWLIST.iter().any(|f| *f == rel) {
        return write_response(
            writer,
            403,
            "text/plain; charset=utf-8",
            b"PUT not allowed for this path",
            false,
        );
    }

    let safe = match safe_join(&cfg.dir, &rel) {
        Some(p) => p,
        None => return write_response(writer, 403, "text/plain; charset=utf-8", b"forbidden", false),
    };

    // Read body — must have a Content-Length. We don't support chunked
    // transfer encoding; browsers' fetch() PUT with a string body always
    // sends Content-Length, so the app is fine.
    let len = match req.headers.get("content-length").and_then(|v| v.parse::<usize>().ok()) {
        Some(n) => n,
        None => return write_response(writer, 411, "text/plain; charset=utf-8", b"length required", false),
    };
    if len > MAX_BODY_BYTES {
        return write_response(writer, 413, "text/plain; charset=utf-8", b"payload too large", false);
    }

    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).map_err(|e| {
        io::Error::new(io::ErrorKind::UnexpectedEof, format!("body read failed: {e}"))
    })?;

    // Validate JSON shape lightly. The app sends pretty-printed JSON; we
    // don't need to parse it fully, but we can sanity-check that it starts
    // with '{' and parses as JSON-ish. Skipping a full parser keeps this
    // dependency-free. The browser app already validates structure on load.
    if !looks_like_json_object(&body) {
        return write_response(writer, 400, "text/plain; charset=utf-8", b"body must be a JSON object", false);
    }

    // Atomic write: write to a temp file in the same directory, then rename.
    // Same-directory rename is atomic on POSIX and on NTFS.
    let dir = safe.parent().unwrap_or(&cfg.dir);
    let tmp = dir.join(format!(".{}.tmp.{}", rel, std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, &safe) {
        let _ = fs::remove_file(&tmp);
        return write_response(
            writer,
            500,
            "text/plain; charset=utf-8",
            format!("rename failed: {e}").as_bytes(),
            false,
        );
    }

    // 204 No Content matches what most JSON APIs return for PUT-succeeded.
    // The Flashcards app only checks `r.ok`, so 204 works perfectly.
    write_response(writer, 204, "", b"", false)
}

fn looks_like_json_object(body: &[u8]) -> bool {
    // Skip leading whitespace and BOM.
    let mut i = 0;
    if body.starts_with(&[0xEF, 0xBB, 0xBF]) {
        i = 3;
    }
    while i < body.len() && matches!(body[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    body.get(i).copied() == Some(b'{')
}

// ───────────────────────────────────────────────────────────────────────────
// Path safety
// ───────────────────────────────────────────────────────────────────────────

/// Join `rel` onto `base`, refusing anything that would escape the base
/// directory (via `..`, absolute paths, or weird components).
fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    // Reject obvious badness up front.
    if rel.is_empty() {
        return Some(base.to_path_buf());
    }
    let candidate = Path::new(rel);
    // Absolute paths are not allowed.
    if candidate.is_absolute() {
        return None;
    }
    // Walk components; refuse "..".
    let mut out = base.to_path_buf();
    for c in candidate.components() {
        match c {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {} // skip "."
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    // Defensive: ensure final path is still inside base.
    // We don't canonicalize because the file may not exist yet (PUT to new file),
    // but the component check above already guarantees no escape.
    Some(out)
}

// ───────────────────────────────────────────────────────────────────────────
// MIME types
// ───────────────────────────────────────────────────────────────────────────

fn mime_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "txt" | "md" => "text/plain; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Response writing
// ───────────────────────────────────────────────────────────────────────────

fn write_response<W: Write>(
    w: &mut W,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> io::Result<()> {
    write_response_ex(w, status, content_type, body, head_only, &[])
}

fn write_response_ex<W: Write>(
    w: &mut W,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    extra: &[(&str, &str)],
) -> io::Result<()> {
    let reason = status_reason(status);
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    head.push_str("Server: kanaweb-server\r\n");
    head.push_str("Connection: close\r\n");
    head.push_str("Cache-Control: no-store\r\n");
    if !content_type.is_empty() {
        head.push_str(&format!("Content-Type: {content_type}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");

    w.write_all(head.as_bytes())?;
    if !head_only && !body.is_empty() {
        w.write_all(body)?;
    }
    w.flush()
}

fn status_reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    }
}
