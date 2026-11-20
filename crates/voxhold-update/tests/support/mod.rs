//! Minimal loopback HTTP server for update tests.
//!
//! It supports `Range`, one-shot connection interruption and request
//! recording. Plain HTTP is used only on 127.0.0.1, matching the URL policy.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Default, Clone)]
pub struct Route {
    pub body: Vec<u8>,
    pub status: u16,
    pub first_status: Option<u16>,
    pub interrupt_after: Option<usize>,
    /// Interrupt every response, not only the first one.
    pub interrupt_every: bool,
    pub allow_range: bool,
}

#[derive(Clone, Debug)]
pub struct RequestRecord {
    pub path: String,
    pub range: Option<String>,
    pub sent: usize,
    pub status: u16,
}

pub struct TestServer {
    pub base_url: String,
    address: SocketAddr,
    requests: Arc<Mutex<Vec<RequestRecord>>>,
    shutdown: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    pub fn start(routes: HashMap<String, Route>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("local address");
        let routes = Arc::new(routes);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let counters: Arc<Mutex<HashMap<String, usize>>> = Arc::new(Mutex::new(HashMap::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();
        let thread_requests = requests.clone();
        // A blocking accept is deterministic: the polling version could miss a
        // freshly queued connection under a loaded CI runner. Drop wakes the
        // loop with one empty connection and joins it.
        let handle = thread::spawn(move || loop {
            if thread_shutdown.load(Ordering::SeqCst) {
                break;
            }
            let (stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(_) => break,
            };
            let routes = routes.clone();
            let requests = thread_requests.clone();
            let counters = counters.clone();
            thread::spawn(move || {
                handle_connection(stream, &routes, &requests, &counters);
            });
        });
        Self {
            base_url: format!("http://{address}"),
            address,
            requests,
            shutdown,
            handle: Some(handle),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn requests(&self) -> Vec<RequestRecord> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the blocking accept. The handler thread returns immediately on
        // an empty request; the accept loop sees the shutdown flag next.
        let _ = TcpStream::connect_timeout(&self.address, Duration::from_millis(200));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    routes: &HashMap<String, Route>,
    requests: &Mutex<Vec<RequestRecord>>,
    counters: &Mutex<HashMap<String, usize>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if buffer.windows(4).any(|window| window == b"\r\n\r\n") || buffer.len() > 16 * 1024
                {
                    break;
                }
            }
            Err(_) => return,
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or("/").to_string();
    let mut range = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("range") {
                range = Some(value.trim().to_string());
            }
        }
    }

    let route = routes.get(&path).cloned().unwrap_or(Route {
        status: 404,
        ..Route::default()
    });
    let first = {
        let mut counters = counters.lock().expect("counter lock");
        let counter = counters.entry(path.clone()).or_insert(0);
        let first = *counter == 0;
        *counter += 1;
        first
    };

    let status = if first {
        route.first_status.unwrap_or(route.status)
    } else {
        route.status
    };
    if status != 0 && status != 200 {
        let status_text = status_text(status);
        let response = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            status, status_text
        );
        let _ = stream.write_all(response.as_bytes());
        record(requests, &path, range, 0, status);
        return;
    }

    let mut start = 0usize;
    if route.allow_range {
        if let Some(value) = &range {
            if let Some(parsed) = parse_range_start(value) {
                start = parsed;
            }
        }
    }
    if start >= route.body.len() {
        let response = format!(
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            route.body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        record(requests, &path, range, 0, 416);
        return;
    }
    let status = if start > 0 { 206 } else { 200 };
    let body = &route.body[start..];
    let mut headers = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
        status_text(status),
        body.len()
    );
    if status == 206 {
        headers.push_str(&format!(
            "Content-Range: bytes {}-{}/{}\r\n",
            start,
            route.body.len() - 1,
            route.body.len()
        ));
    }
    headers.push_str("\r\n");
    if stream.write_all(headers.as_bytes()).is_err() {
        return;
    }
    let sent = if first || route.interrupt_every {
        match route.interrupt_after {
            Some(limit) if limit < body.len() => {
                let _ = stream.write_all(&body[..limit]);
                let _ = stream.flush();
                record(requests, &path, range, limit, status);
                return;
            }
            _ => {
                let _ = stream.write_all(body);
                body.len()
            }
        }
    } else {
        let _ = stream.write_all(body);
        body.len()
    };
    let _ = stream.flush();
    record(requests, &path, range, sent, status);
}

fn record(
    requests: &Mutex<Vec<RequestRecord>>,
    path: &str,
    range: Option<String>,
    sent: usize,
    status: u16,
) {
    requests.lock().expect("requests lock").push(RequestRecord {
        path: path.to_string(),
        range,
        sent,
        status,
    });
}

fn parse_range_start(value: &str) -> Option<usize> {
    let value = value.trim();
    let rest = value.strip_prefix("bytes=")?;
    let (start, _end) = rest.split_once('-')?;
    start.trim().parse::<usize>().ok()
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        _ => "Error",
    }
}
