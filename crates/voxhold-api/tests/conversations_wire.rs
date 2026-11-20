//! Wire-contract tests for the personal conversation API. A local recording
//! server checks the exact method, path, query and body; no backend is touched.

use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use voxhold_api::{Client, Error};

#[derive(Clone, Debug)]
struct Recorded {
    method: String,
    path: String,
    body: String,
}

struct Wire {
    base: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Wire {
    fn spawn(replies: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let recorded = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let running = stop.clone();
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
                let Some((status, body)) = replies.get(index).cloned() else {
                    let _ = write_reply(&mut stream, 500, "{\"error\":\"unexpected\"}");
                    continue;
                };
                let _ = write_reply(&mut stream, status, &body);
            }
        });
        Self {
            base,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(address) = self.base["http://".len()..].parse::<std::net::SocketAddr>() {
            let _ = std::net::TcpStream::connect(address);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn write_reply(stream: &mut std::net::TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Recorded> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
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
    let mut length = 0usize;
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().ok()?;
            }
        }
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Some(Recorded {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn api_client(wire: &Wire) -> Client {
    let mut client = Client::new(&wire.base).unwrap();
    client.set_token("wire".into());
    client
}

fn conversation_json(id: i64) -> String {
    json!({
        "id": id,
        "name": "",
        "direct": true,
        "members": [{"user_id": 1, "username": "me"}],
        "saved": false,
        "muted": false,
        "last_message_id": 0,
        "unread_count": 0
    })
    .to_string()
}

#[test]
fn paged_index_accepts_the_array_and_object_contracts() {
    let array = Wire::spawn(vec![(200, format!("[{}]", conversation_json(7)))]);
    run(async {
        let page = api_client(&array)
            .conversations_page(30, None)
            .await
            .unwrap();
        assert_eq!(page.conversations.len(), 1);
        assert_eq!(page.conversations[0].id, 7);
        assert!(!page.has_more, "a bare array is a complete first page");
        assert!(page.next_cursor.is_none());
    });
    let requests = array.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/api/v1/me/conversations?limit=30");

    let object = Wire::spawn(vec![(
        200,
        json!({
            "conversations": [serde_json::from_str::<serde_json::Value>(&conversation_json(9)).unwrap()],
            "has_more": true,
            "next_cursor": "abc-1_x"
        })
        .to_string(),
    )]);
    run(async {
        let page = api_client(&object)
            .conversations_page(30, Some("prev_cursor"))
            .await
            .unwrap();
        assert_eq!(page.conversations.len(), 1);
        assert_eq!(page.conversations[0].id, 9);
        assert!(page.has_more);
        assert_eq!(page.next_cursor.as_deref(), Some("abc-1_x"));
    });
    let requests = object.requests();
    assert_eq!(
        requests[0].path,
        "/api/v1/me/conversations?limit=30&before=prev_cursor"
    );
}

#[test]
fn paged_index_rejects_invalid_limits_cursors_and_rows() {
    // No server: every call must fail before reaching the network.
    let wire = Wire::spawn(Vec::new());
    let client = api_client(&wire);
    run(async {
        assert!(matches!(
            client.conversations_page(0, None).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.conversations_page(201, None).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.conversations_page(30, Some("")).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.conversations_page(30, Some("bad cursor")).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.conversations_page(30, Some("a+b")).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.conversations_page(30, Some(&"a".repeat(97))).await,
            Err(Error::InvalidResponse(_))
        ));
    });
    assert!(
        wire.requests().is_empty(),
        "validation must precede the GET"
    );

    let missing = Wire::spawn(vec![(
        200,
        json!({"conversations": [], "has_more": true, "next_cursor": null}).to_string(),
    )]);
    run(async {
        assert!(matches!(
            api_client(&missing).conversations_page(30, None).await,
            Err(Error::InvalidResponse(_))
        ));
    });

    let repeated = Wire::spawn(vec![(
        200,
        json!({"conversations": [], "has_more": true, "next_cursor": "same"}).to_string(),
    )]);
    run(async {
        assert!(matches!(
            api_client(&repeated)
                .conversations_page(30, Some("same"))
                .await,
            Err(Error::InvalidResponse(_))
        ));
    });

    let bad_row = Wire::spawn(vec![(
        200,
        json!({"conversations": [{"id": 0}], "has_more": false, "next_cursor": null}).to_string(),
    )]);
    run(async {
        assert!(matches!(
            api_client(&bad_row).conversations_page(30, None).await,
            Err(Error::InvalidResponse(_))
        ));
    });
}

#[test]
fn message_search_encodes_the_query_and_numeric_before() {
    let empty = json!({"results": [], "has_more": false, "next_before": null}).to_string();
    let wire = Wire::spawn(vec![(200, empty.clone()), (200, empty)]);
    run(async {
        let client = api_client(&wire);
        client.search_conversations("не ва&x=1", 42).await.unwrap();
        client.search_conversations("проект", 0).await.unwrap();
    });
    let requests = wire.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].path,
        "/api/v1/me/conversations/search?q=%D0%BD%D0%B5+%D0%B2%D0%B0%26x%3D1&before=42"
    );
    assert_eq!(
        requests[1].path, "/api/v1/me/conversations/search?q=%D0%BF%D1%80%D0%BE%D0%B5%D0%BA%D1%82",
        "zero before is omitted"
    );
}

#[test]
fn message_search_rejects_invalid_queries_and_cursors() {
    let wire = Wire::spawn(Vec::new());
    let client = api_client(&wire);
    run(async {
        assert!(matches!(
            client.search_conversations("  ", 0).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.search_conversations(&"я".repeat(201), 0).await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.search_conversations("ok", -1).await,
            Err(Error::InvalidResponse(_))
        ));
    });
    assert!(wire.requests().is_empty());

    let bad_hit = Wire::spawn(vec![(
        200,
        json!({
            "results": [{"id": 0, "conversation_id": 7}],
            "has_more": false,
            "next_before": null
        })
        .to_string(),
    )]);
    run(async {
        assert!(matches!(
            api_client(&bad_hit).search_conversations("ok", 0).await,
            Err(Error::InvalidResponse(_))
        ));
    });

    let bad_cursor = Wire::spawn(vec![(
        200,
        json!({
            "results": [{"id": 3, "conversation_id": 7}],
            "has_more": true,
            "next_before": 3
        })
        .to_string(),
    )]);
    run(async {
        assert!(
            matches!(
                api_client(&bad_cursor).search_conversations("ok", 3).await,
                Err(Error::InvalidResponse(_))
            ),
            "a non-decreasing before cursor would repeat the page"
        );
    });
}

#[test]
fn conversation_get_checks_the_answer_id_and_input() {
    let wire = Wire::spawn(vec![(200, conversation_json(7))]);
    run(async {
        let row = api_client(&wire).conversation(7).await.unwrap();
        assert_eq!(row.id, 7);
    });
    assert_eq!(wire.requests()[0].path, "/api/v1/conversations/7");

    let mismatch = Wire::spawn(vec![(200, conversation_json(8))]);
    run(async {
        assert!(matches!(
            api_client(&mismatch).conversation(7).await,
            Err(Error::InvalidResponse(_))
        ));
    });

    let none = Wire::spawn(Vec::new());
    run(async {
        assert!(matches!(
            api_client(&none).conversation(0).await,
            Err(Error::InvalidResponse(_))
        ));
    });
    assert!(none.requests().is_empty());
}

#[test]
fn create_body_matches_the_backend_contract() {
    let wire = Wire::spawn(vec![(200, conversation_json(7))]);
    run(async {
        let row = api_client(&wire)
            .create_conversation(&[2, 3], "  Проект  ")
            .await
            .unwrap();
        assert_eq!(row.id, 7);
    });
    let requests = wire.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/api/v1/me/conversations");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&requests[0].body).unwrap(),
        json!({"user_ids": [2, 3], "name": "Проект"})
    );

    let none = Wire::spawn(Vec::new());
    let client = api_client(&none);
    run(async {
        assert!(matches!(
            client.create_conversation(&[], "").await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.create_conversation(&[2, 2], "").await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.create_conversation(&[0], "").await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client
                .create_conversation(&(2..18).collect::<Vec<_>>(), "")
                .await,
            Err(Error::InvalidResponse(_))
        ));
        assert!(matches!(
            client.create_conversation(&[2], &"я".repeat(65)).await,
            Err(Error::InvalidResponse(_))
        ));
    });
    assert!(none.requests().is_empty(), "invalid bodies never POST");
}

#[test]
fn notification_get_and_put_use_the_conversation_path() {
    let wire = Wire::spawn(vec![
        (
            200,
            json!({"conversation_id": 7, "muted": true}).to_string(),
        ),
        (
            200,
            json!({"conversation_id": 7, "muted": false}).to_string(),
        ),
    ]);
    run(async {
        let client = api_client(&wire);
        assert!(client.conversation_notification(7).await.unwrap().muted);
        assert!(
            !client
                .set_conversation_notification(7, false)
                .await
                .unwrap()
                .muted
        );
    });
    let requests = wire.requests();
    assert_eq!(requests[0].path, "/api/v1/conversations/7/notifications");
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[1].path, "/api/v1/conversations/7/notifications");
    assert_eq!(requests[1].method, "PUT");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&requests[1].body).unwrap(),
        json!({"muted": false})
    );

    let mismatch = Wire::spawn(vec![(
        200,
        json!({"conversation_id": 8, "muted": true}).to_string(),
    )]);
    run(async {
        assert!(matches!(
            api_client(&mismatch).conversation_notification(7).await,
            Err(Error::InvalidResponse(_))
        ));
    });

    let none = Wire::spawn(Vec::new());
    run(async {
        assert!(matches!(
            api_client(&none)
                .set_conversation_notification(0, true)
                .await,
            Err(Error::InvalidResponse(_))
        ));
    });
    assert!(none.requests().is_empty());
}

#[test]
fn leave_deletes_the_membership_path_and_surfaces_http_errors() {
    let wire = Wire::spawn(vec![
        (200, String::new()),
        (404, "{\"error\":\"missing\"}".into()),
    ]);
    run(async {
        let client = api_client(&wire);
        client.leave_conversation(7).await.unwrap();
        assert!(matches!(
            client.leave_conversation(9).await,
            Err(Error::Http(404))
        ));
    });
    let requests = wire.requests();
    assert_eq!(requests[0].method, "DELETE");
    assert_eq!(requests[0].path, "/api/v1/conversations/7/members/me");
    assert_eq!(requests[1].path, "/api/v1/conversations/9/members/me");

    let none = Wire::spawn(Vec::new());
    run(async {
        assert!(matches!(
            api_client(&none).leave_conversation(0).await,
            Err(Error::InvalidResponse(_))
        ));
    });
    assert!(none.requests().is_empty());
}
