//! Two instances on different ports must never see each other's Bearer: the
//! first frame of the voice socket is the only place the token travels over WS,
//! so each real peer records exactly the token of its own client.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;
use voxhold_api::Client;

struct Peer {
    base: String,
    address: std::net::SocketAddr,
    frames: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Peer {
    fn start() -> Self {
        let (tx, rx) = mpsc::channel::<std::net::SocketAddr>();
        let frames = Arc::new(Mutex::new(Vec::new()));
        let seen = frames.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let running = stop.clone();
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind ws peer");
                tx.send(listener.local_addr().unwrap()).expect("address");
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    if running.load(Ordering::SeqCst) {
                        break;
                    }
                    let seen = seen.clone();
                    tokio::spawn(async move {
                        let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                            return;
                        };
                        while let Some(Ok(message)) = socket.next().await {
                            if let Message::Text(text) = message {
                                seen.lock().unwrap().push(text);
                            }
                        }
                    });
                }
            });
        });
        let address = rx.recv_timeout(Duration::from_secs(5)).expect("address");
        Self {
            base: format!("http://{address}"),
            address,
            frames,
            stop,
            handle: Some(handle),
        }
    }

    fn frames(&self) -> Vec<String> {
        self.frames.lock().unwrap().clone()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop so the thread can leave.
        let _ = std::net::TcpStream::connect(self.address);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn auth_token(frames: &[String]) -> String {
    let first = frames.first().expect("the peer saw the auth frame");
    let value: Value = serde_json::from_str(first).expect("json frame");
    assert_eq!(value["type"], "auth", "{first}");
    value["data"]["token"]
        .as_str()
        .expect("token string")
        .to_owned()
}

#[test]
fn each_origin_sends_only_its_own_token_in_the_first_ws_frame() {
    let first = Peer::start();
    let second = Peer::start();
    assert_ne!(first.base, second.base, "two different ports");

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let mut client_a = Client::new(&first.base).expect("first origin");
        client_a.set_token("token-A".into());
        let mut client_b = Client::new(&second.base).expect("second origin");
        client_b.set_token("token-B".into());

        let socket_a = client_a.open_voice_socket().await.expect("first socket");
        let socket_b = client_b.open_voice_socket().await.expect("second socket");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !first.frames().is_empty() && !second.frames().is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "the auth frames did not arrive");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        drop(socket_a);
        drop(socket_b);
    });

    let first_frames = first.frames();
    let second_frames = second.frames();
    assert_eq!(auth_token(&first_frames), "token-A");
    assert_eq!(auth_token(&second_frames), "token-B");
    for frame in &first_frames {
        assert!(
            !frame.contains("token-B"),
            "first peer saw token-B: {frame}"
        );
    }
    for frame in &second_frames {
        assert!(
            !frame.contains("token-A"),
            "second peer saw token-A: {frame}"
        );
    }
}
