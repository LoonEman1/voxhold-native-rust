//! A tiny blocking HTTP server for transport tests. It records one request at
//! a time and answers with the queued responses, so streaming, headers and
//! retries are checked without a real backend.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

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
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    pub fn new(status: u16, body: &str) -> Self {
        Self {
            status,
            body: body.to_string(),
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
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let recorded = requests.clone();
        let running = stop.clone();
        let handle = std::thread::spawn(move || {
            let replies = replies;
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
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        reply.status,
                        reply.body.len(),
                        reply.body
                    )
                    .as_bytes(),
                );
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
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < expected {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Recorded {
        method,
        path,
        headers,
        body,
    })
}
