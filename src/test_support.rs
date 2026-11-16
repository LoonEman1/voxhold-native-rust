//! A tiny blocking HTTP server for transport tests. It records one request at
//! a time and answers with the queued responses, so streaming, headers and
//! retries are checked without a real backend.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};

/// Holds one response until the test releases it. `arm` makes only the next
/// gated requests block, so setup requests (history, members) are not caught.
#[derive(Default)]
pub struct Gate {
    armed: AtomicBool,
    arrived: (Mutex<bool>, Condvar),
    released: (Mutex<bool>, Condvar),
}

impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn arm(&self) {
        *self.arrived.0.lock().unwrap() = false;
        *self.released.0.lock().unwrap() = false;
        self.armed.store(true, Ordering::SeqCst);
    }

    pub fn release(&self) {
        self.armed.store(false, Ordering::SeqCst);
        *self.released.0.lock().unwrap() = true;
        self.released.1.notify_all();
    }

    /// Wait until a gated request reached the server.
    pub fn wait_arrived(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut arrived = self.arrived.0.lock().unwrap();
        while !*arrived {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            let (next, result) = self.arrived.1.wait_timeout(arrived, left).unwrap();
            arrived = next;
            if result.timed_out() && !*arrived {
                return false;
            }
        }
        true
    }

    fn block_if_armed(&self) {
        if !self.armed.load(Ordering::SeqCst) {
            return;
        }
        *self.arrived.0.lock().unwrap() = true;
        self.arrived.1.notify_all();
        let mut released = self.released.0.lock().unwrap();
        while !*released {
            released = self
                .released
                .1
                .wait_timeout(released, Duration::from_secs(10))
                .unwrap()
                .0;
            if !*released {
                break;
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Debug)]
pub enum ReplyBody {
    Bytes(Vec<u8>),
    /// Stream from disk in 64 KiB chunks so a large download never sits in the
    /// test server's heap.
    File(std::path::PathBuf),
}

#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub body: ReplyBody,
    pub content_type: &'static str,
    pub gate: Option<Arc<Gate>>,
    /// Hold the response for a while before writing headers. Used by the
    /// long-recording test that must outlive the eight-second JSON timeout.
    pub delay: Option<Duration>,
}

impl Reply {
    pub fn new(status: u16, body: &str) -> Self {
        Self {
            status,
            body: ReplyBody::Bytes(body.as_bytes().to_vec()),
            content_type: "application/json",
            gate: None,
            delay: None,
        }
    }

    /// Binary response used by the picture endpoints.
    pub fn image(status: u16, body: Vec<u8>) -> Self {
        Self {
            status,
            body: ReplyBody::Bytes(body),
            content_type: "image/png",
            gate: None,
            delay: None,
        }
    }

    /// Stream a file response with the real `Content-Length`.
    pub fn file(status: u16, path: std::path::PathBuf, content_type: &'static str) -> Self {
        Self {
            status,
            body: ReplyBody::File(path),
            content_type,
            gate: None,
            delay: None,
        }
    }

    pub fn with_gate(mut self, gate: Arc<Gate>) -> Self {
        self.gate = Some(gate);
        self
    }

    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }

    fn content_length(&self) -> u64 {
        match &self.body {
            ReplyBody::Bytes(bytes) => bytes.len() as u64,
            ReplyBody::File(path) => std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
        }
    }
}

pub struct TestServer {
    pub base: String,
    pub requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    pub fn spawn(replies: Vec<Reply>) -> Self {
        Self::start(replies, false)
    }

    /// Thread-per-connection server for tests that must hold two requests at
    /// the same time. `spawn` keeps the deterministic single-threaded order.
    pub fn spawn_concurrent(replies: Vec<Reply>) -> Self {
        Self::start(replies, true)
    }

    fn start(replies: Vec<Reply>, concurrent: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let recorded = requests.clone();
        let running = stop.clone();
        let replies = Arc::new(replies);
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if running.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(mut stream) = stream else { continue };
                let Some(record) = read_request(&mut stream) else {
                    continue;
                };
                let index = {
                    let mut records = recorded.lock().unwrap();
                    records.push(record);
                    records.len() - 1
                };
                let reply = replies
                    .get(index)
                    .cloned()
                    .unwrap_or(Reply::new(500, "{\"error\":\"unexpected request\"}"));
                if concurrent {
                    let running = running.clone();
                    std::thread::spawn(move || {
                        if running.load(Ordering::Relaxed) {
                            return;
                        }
                        write_reply(&mut stream, &reply);
                    });
                } else {
                    write_reply(&mut stream, &reply);
                }
            }
        });
        Self {
            base,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

/// Write one queued answer. Called on the accept thread for the deterministic
/// server and on a connection thread for the concurrent one.
fn write_reply(stream: &mut std::net::TcpStream, reply: &Reply) {
    if let Some(delay) = reply.delay {
        std::thread::sleep(delay);
    }
    if let Some(gate) = &reply.gate {
        gate.block_if_armed();
    }
    let mut response = format!(
        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        reply.status,
        reply.content_type,
        reply.content_length()
    )
    .into_bytes();
    let _ = stream.write_all(&response);
    match &reply.body {
        ReplyBody::Bytes(bytes) => {
            let _ = stream.write_all(bytes);
        }
        ReplyBody::File(path) => {
            if let Ok(mut file) = std::fs::File::open(path) {
                let mut chunk = vec![0u8; 64 << 10];
                loop {
                    use std::io::Read;
                    match file.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            if stream.write_all(&chunk[..read]).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake the accept loop with one connection so the thread can exit.
        if let Ok(address) = self.base["http://".len()..].parse::<std::net::SocketAddr>() {
            let _ = std::net::TcpStream::connect(address);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Recorded> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .ok()?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        if buffer.len() > 64 << 10 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split_whitespace();
    let method = request_line.next()?.to_string();
    let path = request_line.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    let expected: usize = headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    // Keep the memory measurement honest: a large upload is drained in 8 KiB
    // chunks and only the first 4 KiB are recorded for assertions.
    const CAPTURE: usize = 1 << 20;
    let mut body = buffer[header_end + 4..].to_vec();
    if expected <= CAPTURE {
        while body.len() < expected {
            let read = stream.read(&mut chunk).ok()?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
    } else {
        body.truncate(4096);
        let mut seen = buffer.len() - header_end - 4;
        while seen < expected {
            let read = stream.read(&mut chunk).ok()?;
            if read == 0 {
                break;
            }
            seen += read;
        }
    }
    Some(Recorded {
        method,
        path,
        headers,
        body,
    })
}

/// Private bytes of the current process (Windows `PROCESS_MEMORY_COUNTERS`).
/// The memory-measurement test uses it before, during and after a transfer.
#[cfg(windows)]
pub fn private_bytes() -> u64 {
    #[repr(C)]
    #[derive(Default)]
    struct Counters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut core::ffi::c_void;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut core::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }

    let mut counters = Counters::default();
    let size = std::mem::size_of::<Counters>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    if ok == 0 {
        0
    } else {
        counters.pagefile_usage as u64
    }
}

#[cfg(not(windows))]
pub fn private_bytes() -> u64 {
    0
}
