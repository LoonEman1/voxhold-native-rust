//! Chat-socket heartbeat: the account socket must prove that the server still
//! answers. One `ping` is outstanding at a time, a matching `pong` clears it,
//! a foreign `pong` and ordinary traffic do not extend the deadline, and a
//! silent server ends the subscription with `Error::Network` so the outer
//! reconnect loop can open a fresh socket. Short intervals keep the tests fast.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use voxhold_api::{ChannelSubscription, Client, Error, HeartbeatConfig, RealtimeEvent};

type Peer = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

async fn read_frame(peer: &mut Peer) -> RealtimeEvent {
    loop {
        let message = peer.next().await.expect("frame").expect("websocket");
        match message {
            Message::Text(text) => return serde_json::from_str(&text).expect("json frame"),
            Message::Binary(bytes) => serde_json::from_slice(&bytes).expect("json frame"),
            _ => {}
        }
    }
}

async fn send_frame(peer: &mut Peer, value: Value) {
    peer.send(Message::Text(value.to_string().into()))
        .await
        .expect("send frame");
}

async fn accept(listener: &TcpListener) -> Peer {
    let (stream, _) = timeout(Duration::from_secs(2), listener.accept())
        .await
        .expect("accept")
        .expect("connection");
    let mut peer = timeout(
        Duration::from_secs(2),
        tokio_tungstenite::accept_async(stream),
    )
    .await
    .expect("handshake")
    .expect("websocket");
    let auth = read_frame(&mut peer).await;
    assert_eq!(auth.kind, "auth");
    send_frame(&mut peer, json!({"type": "ready"})).await;
    peer
}

/// A socket that stops answering after `ready` must fail by the heartbeat
/// deadline instead of staying silent forever.
#[tokio::test]
async fn a_silent_socket_fails_by_the_heartbeat_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let mut client =
        Client::new(&format!("http://{}", listener.local_addr().unwrap())).expect("client");
    client.set_token("fixture".into());
    let (target, desired) = watch::channel(None);
    let (events, _received) = mpsc::unbounded_channel();
    let worker = tokio::spawn(async move {
        client
            .subscribe_channels_with(
                desired,
                move |event| {
                    let _ = events.send(event);
                },
                HeartbeatConfig {
                    interval: Duration::from_millis(120),
                    timeout: Duration::from_millis(120),
                },
            )
            .await
    });

    let mut peer = accept(&listener).await;
    let ping = timeout(Duration::from_secs(2), read_frame(&mut peer))
        .await
        .expect("the first ping arrives after the interval");
    assert_eq!(ping.kind, "ping");
    assert!(!ping.request_id.is_empty(), "ping carries a request id");

    let started = Instant::now();
    let result = timeout(Duration::from_secs(2), worker)
        .await
        .expect("the subscription ends by the deadline")
        .expect("worker joins");
    assert!(
        matches!(result, Err(Error::Network)),
        "a silent socket is a network error: {result:?}"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(120),
        "the deadline is measured from the ping, took {:?}",
        started.elapsed()
    );
    drop(target);
}

/// A matching `pong` clears the one outstanding ping, is not forwarded to the
/// UI, and the next ping is only sent after the interval.
#[tokio::test]
async fn a_matching_pong_clears_the_heartbeat_wait_and_schedules_the_next_ping() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let mut client =
        Client::new(&format!("http://{}", listener.local_addr().unwrap())).expect("client");
    client.set_token("fixture".into());
    let (target, desired) = watch::channel(None);
    let (events, mut received) = mpsc::unbounded_channel();
    let interval = Duration::from_millis(150);
    let worker = tokio::spawn(async move {
        client
            .subscribe_channels_with(
                desired,
                move |event| {
                    let _ = events.send(event);
                },
                HeartbeatConfig {
                    interval,
                    timeout: Duration::from_millis(500),
                },
            )
            .await
    });

    let mut peer = accept(&listener).await;
    let ping = timeout(Duration::from_secs(2), read_frame(&mut peer))
        .await
        .expect("first ping");
    assert_eq!(ping.kind, "ping");
    let answered = Instant::now();
    send_frame(
        &mut peer,
        json!({"type": "pong", "request_id": ping.request_id}),
    )
    .await;

    let next = timeout(Duration::from_secs(2), read_frame(&mut peer))
        .await
        .expect("the cleared wait does not block the next ping");
    assert_eq!(next.kind, "ping");
    assert_ne!(next.request_id, ping.request_id, "each ping has its own id");
    assert!(
        answered.elapsed() >= interval.mul_f32(0.6),
        "the next ping waits for the interval, took {:?}",
        answered.elapsed()
    );

    // The pong itself must never reach the UI: only `ready` is delivered.
    assert_eq!(received.try_recv().expect("ready").kind, "ready");
    assert!(
        received.try_recv().is_err(),
        "neither ping nor pong is forwarded to the receiver"
    );

    drop(target);
    let result = timeout(Duration::from_secs(2), worker)
        .await
        .expect("the worker stops when the subscription channel closes")
        .expect("worker joins");
    assert!(result.is_ok());
}

/// A `pong` for another request id and normal chat traffic must not satisfy or
/// postpone the deadline.
#[tokio::test]
async fn a_foreign_pong_and_chat_events_do_not_extend_the_heartbeat_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let mut client =
        Client::new(&format!("http://{}", listener.local_addr().unwrap())).expect("client");
    client.set_token("fixture".into());
    let (target, desired) = watch::channel(None);
    let (events, mut received) = mpsc::unbounded_channel();
    let worker = tokio::spawn(async move {
        client
            .subscribe_channels_with(
                desired,
                move |event| {
                    let _ = events.send(event);
                },
                HeartbeatConfig {
                    interval: Duration::from_millis(100),
                    timeout: Duration::from_millis(150),
                },
            )
            .await
    });

    let mut peer = accept(&listener).await;
    let ping = timeout(Duration::from_secs(2), read_frame(&mut peer))
        .await
        .expect("first ping");
    assert_eq!(ping.kind, "ping");
    send_frame(
        &mut peer,
        json!({"type": "pong", "request_id": "someone-else"}),
    )
    .await;
    send_frame(
        &mut peer,
        json!({"type": "presence.updated", "data": {"user_id": 4, "status": "online"}}),
    )
    .await;

    let started = Instant::now();
    let result = timeout(Duration::from_secs(2), worker)
        .await
        .expect("a foreign pong does not cancel the deadline")
        .expect("worker joins");
    assert!(
        matches!(result, Err(Error::Network)),
        "the request was never answered: {result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the failure comes from the ping deadline, not the test timeout"
    );

    let ready = received.try_recv().expect("ready");
    assert_eq!(ready.kind, "ready");
    let presence = received.try_recv().expect("ordinary event");
    assert_eq!(presence.kind, "presence.updated");
    assert!(
        received.try_recv().is_err(),
        "no pong is forwarded to the receiver"
    );
    drop(target);
}

/// While a ping waits for its `pong`, a channel change still sends the
/// unsubscribe/subscribe pair: the heartbeat timer must not starve the
/// subscription branch of the select.
#[tokio::test]
async fn a_channel_change_during_a_pending_heartbeat_pong_still_resubscribes() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let mut client =
        Client::new(&format!("http://{}", listener.local_addr().unwrap())).expect("client");
    client.set_token("fixture".into());
    let first = ChannelSubscription {
        server_id: 1,
        channel_id: 10,
        generation: 1,
    };
    let second = ChannelSubscription {
        server_id: 1,
        channel_id: 11,
        generation: 2,
    };
    let (target, desired) = watch::channel(Some(first));
    let (events, _received) = mpsc::unbounded_channel();
    let worker = tokio::spawn(async move {
        client
            .subscribe_channels_with(
                desired,
                move |event| {
                    let _ = events.send(event);
                },
                HeartbeatConfig {
                    interval: Duration::from_millis(100),
                    timeout: Duration::from_millis(800),
                },
            )
            .await
    });

    let mut peer = accept(&listener).await;
    let subscribe = timeout(Duration::from_secs(2), read_frame(&mut peer))
        .await
        .expect("initial subscribe");
    assert_eq!(subscribe.kind, "channel.subscribe");
    assert_eq!(subscribe.data["channel_id"], 10);

    let ping = timeout(Duration::from_secs(2), read_frame(&mut peer))
        .await
        .expect("ping while the channel is subscribed");
    assert_eq!(ping.kind, "ping", "the pong is still outstanding");

    target.send(Some(second)).expect("change the channel");
    let unsubscribe = timeout(Duration::from_secs(2), read_frame(&mut peer))
        .await
        .expect("unsubscribe during the pending pong");
    assert_eq!(unsubscribe.kind, "channel.unsubscribe");
    assert_eq!(unsubscribe.data["channel_id"], 10);
    let subscribe = timeout(Duration::from_secs(2), read_frame(&mut peer))
        .await
        .expect("subscribe during the pending pong");
    assert_eq!(subscribe.kind, "channel.subscribe");
    assert_eq!(subscribe.data["channel_id"], 11);
    assert_eq!(subscribe.request_id, "native-channel-2");

    send_frame(
        &mut peer,
        json!({"type": "pong", "request_id": ping.request_id}),
    )
    .await;
    drop(target);
    let result = timeout(Duration::from_secs(2), worker)
        .await
        .expect("the worker stops when the subscription channel closes")
        .expect("worker joins");
    assert!(result.is_ok());
}
