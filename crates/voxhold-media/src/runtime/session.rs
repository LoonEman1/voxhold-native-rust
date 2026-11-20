//! The per-socket voice session loop and the reconnect/rotation policy.

use super::requests::ScreenCommand;
use super::*;

async fn run_session(
    client: &voxhold_api::Client,
    requests: &Requests,
    controller: &mut VoiceController,
    viewer: &mut Viewer,
    updates: Arc<Updates>,
    frame: Arc<Mutex<Option<StreamFrame>>>,
    capture: Arc<CaptureGate>,
    screen_preview: Arc<crate::screen_capture::PreviewStore>,
) -> SessionEnd {
    let config = match client.webrtc_config().await {
        Ok(config) => config,
        Err(error) => {
            return if controller.resume_connection_id.is_some() {
                SessionEnd::Recover
            } else {
                SessionEnd::Error(format!(
                    "Не удалось загрузить настройки соединения: {error}"
                ))
            };
        }
    };
    let media = match Media::new_with_capture(capture) {
        Ok(media) => media,
        Err(error) => return SessionEnd::Error(format!("{error:?}")),
    };
    let servers: Vec<IceServer> = config
        .ice_servers
        .into_iter()
        .map(|server| IceServer {
            urls: server.urls,
            username: server.username,
            credential: server.credential,
        })
        .collect();
    if let Err(error) = media.command(Command::Start {
        ice_servers: servers.clone(),
        ice_transport_policy: config.ice_transport_policy.clone(),
        controls: controller.effective(),
    }) {
        return SessionEnd::Error(format!("{error:?}"));
    }
    let socket = match client.open_voice_socket().await {
        Ok(socket) => socket,
        Err(error) => {
            return if controller.resume_connection_id.is_some() {
                SessionEnd::Recover
            } else {
                SessionEnd::Error(error.to_string())
            };
        }
    };
    // The token this socket was authorized with. A rotation replaces it while
    // the old socket may still deliver one close; that close is stale.
    let session_token = client.token();
    let cancel_epoch = requests.cancellation_epoch();
    let mut live = Live {
        socket,
        media,
        servers,
        policy: config.ice_transport_policy,
        output: controller.controls.output.clone(),
        output_working: true,
        stream: None,
        stream_selection: None,
        publisher: crate::publisher::Publisher::new(controller.server_id, controller.channel_id),
        screen: None,
        bitrate: screen::BitrateQueue::default(),
        prep: None,
        restart: None,
        server: controller.server_id,
        channel: controller.channel_id,
        connection: controller.resume_connection_id.clone(),
        joined: controller.joined,
        snapshot_ready: false,
        access_seen: false,
        access: ScreenAccess::default(),
        screen_access: requests.screen_access.clone(),
        recovery: requests.publish_recovery.clone(),
        recovery_epoch: cancel_epoch,
        cancel_epoch: requests.publish_cancel_epoch.clone(),
        rotation: requests.rotation.clone(),
        session_token,
        screen_preview,
        render: VideoFrame::default(),
        viewport: requests.viewport(),
        local_level: None,
        frame,
        updates,
    };
    live.publisher
        .set_connection(controller.resume_connection_id.clone());
    live.publisher.set_access(live.access_value());
    let mut tick = tokio::time::interval(Duration::from_millis(25));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        // A viewport change (fullscreen, resize, DPI) wakes the worker through
        // `set_viewport`. Apply it before waiting for a socket event so the
        // latest decoded frame is re-read without a new RTP frame.
        let viewport = requests.viewport();
        if viewport != live.viewport {
            live.viewport = viewport;
            match live.drain_viewer(viewer).await {
                Ok(true) => {}
                Ok(false) => return SessionEnd::Recover,
                Err(end) => return end,
            }
        }
        let notified = requests.notify.notified();
        while let Some(pending) = requests.take() {
            match pending {
                Pending::Leave => {
                    // The handle already latched the capture synchronously.
                    // Stopping the media worker applies the same latch again
                    // and releases the device before the leave handshake.
                    let _ = live.media.command(Command::Stop);
                    // The screen capture has one owner: this attempt. Release
                    // it before the socket can stall on the leave handshake.
                    if let Err(end) = live.abandon_screen(viewer).await {
                        return end;
                    }
                    let outcomes = viewer.leave(Instant::now());
                    let _ = live.apply(outcomes, viewer).await;
                    live.send_leave().await;
                    // A normal leave waits briefly for the acknowledgement so
                    // the server removes the participant before we drop the
                    // peer; the socket is closed regardless.
                    let deadline = Instant::now() + Duration::from_secs(1);
                    while Instant::now() < deadline {
                        let left =
                            tokio::time::timeout(Duration::from_millis(200), live.socket.read());
                        match left.await {
                            Ok(Ok(event)) if event.kind == "voice.left" => break,
                            Ok(Ok(_)) => continue,
                            _ => break,
                        }
                    }
                    live.stream = None;
                    live.stream_selection = None;
                    live.clear_frame();
                    return SessionEnd::Leave;
                }
                Pending::Stream(StreamCommand::Close { selection }) => {
                    // The queue has already rejected a close older than the
                    // newest applied command. A newer close therefore means the
                    // current picture, even when an intermediate Watch(B) was
                    // displaced before the worker read it.
                    let outcomes = viewer.leave_selection(selection, Instant::now());
                    if live.stream_selection == Some(selection) {
                        live.stream = None;
                        live.stream_selection = None;
                        live.clear_frame();
                    }
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::Stream(StreamCommand::Watch {
                    selection,
                    publication,
                }) => {
                    let outcomes = viewer.select(&publication, selection, Instant::now());
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::Screen(command) => {
                    if let Err(end) = live
                        .handle_screen_command(command, controller, viewer)
                        .await
                    {
                        return end;
                    }
                }
                Pending::ScreenSources(request) => live.list_screen_sources(request),
                Pending::ScreenAccess(access) => {
                    // The panel's newest gate is applied before the pending
                    // capture can complete; a revocation cancels it and fails
                    // the attempt instead of letting a late engine start.
                    live.note_access();
                    let outcomes = live.apply_screen_access(access);
                    if let Err(end) = live.apply_screen(outcomes, viewer).await {
                        return end;
                    }
                    if let Err(end) = live.recover_publish(viewer).await {
                        return end;
                    }
                }
                Pending::Output(output) => {
                    live.output = output.clone();
                    if let Some(stream) = &mut live.stream {
                        if let Err(message) = stream.select_output(output) {
                            live.updates.push(VoiceUpdate::StreamStatus {
                                selection: live.stream_selection.unwrap_or_default(),
                                status: message,
                            });
                        }
                    }
                }
                Pending::Input(request) => {
                    // The media engine owns the device switch and reports the
                    // applied input through `InputApplied`. Keep the controller
                    // in sync so a reconnect or the next PTT State does not
                    // revert to the previous selection.
                    let previous = controller.controls.input.clone();
                    let generation = request.generation;
                    if let Err(error) = live.media.command(Command::SelectInput {
                        input: request.input.clone(),
                        generation,
                    }) {
                        live.updates.push(VoiceUpdate::InputApplied {
                            generation,
                            input: previous,
                            error: Some(format!(
                                "Не удалось передать выбор микрофона медиадвижку: {error:?}"
                            )),
                        });
                    } else {
                        controller.controls.input = request.input;
                    }
                }
                Pending::StreamMute(selection, muted) => {
                    let outcomes = viewer.set_muted(selection, muted);
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                }
                Pending::Controls(controls, self_muted) => {
                    let output = controls.output.clone();
                    let actions = controller.update_controls(controls, self_muted);
                    if output != live.output || !live.output_working {
                        live.output.clone_from(&output);
                        if let Some(stream) = &mut live.stream {
                            if let Err(message) = stream.set_output(output.clone()) {
                                live.updates.push(VoiceUpdate::StreamStatus {
                                    selection: live.stream_selection.unwrap_or_default(),
                                    status: message,
                                });
                            }
                        }
                    }
                    match live.execute(actions).await {
                        Ok(true) => {}
                        Ok(false) => return SessionEnd::Recover,
                        Err(end) => return end,
                    }
                }
                Pending::LocalMute(connection, muted) => {
                    let actions = controller.set_local_mute(&connection, muted);
                    match live.execute(actions).await {
                        Ok(true) => {}
                        Ok(false) => return SessionEnd::Recover,
                        Err(end) => return end,
                    }
                    live.updates
                        .push(VoiceUpdate::Tracks(controller.connections_with_audio()));
                    live.updates
                        .push(VoiceUpdate::Speaking(controller.speaking.clone()));
                }
                Pending::Power(resumed) => {
                    let command = if resumed {
                        Command::Resume
                    } else {
                        Command::Suspend
                    };
                    if let Err(error) = live.media.command(command) {
                        live.updates.push(VoiceUpdate::DeviceError(format!(
                            "Не удалось передать состояние питания медиадвижку: {error:?}"
                        )));
                    }
                }
            }
            live.updates
                .push(VoiceUpdate::Participants(controller.participants.clone()));
        }
        tokio::select! {
            _ = notified => {}
            event = live.socket.read() => {
                let Ok(event) = event else { return SessionEnd::Recover };
                if event.kind.starts_with("stream.")
                    || (event.kind == "error"
                        && (event.request_id.starts_with("native-stream-")
                            || event.request_id.starts_with("native-screen-")))
                {
                    let now = Instant::now();
                    let outcomes = viewer.on_server_event(
                        &event.kind,
                        &event.request_id,
                        &event.data,
                        now,
                    );
                    if let Err(end) = live.apply(outcomes, viewer).await {
                        return end;
                    }
                    // The publisher filters by its own attempt; a viewer event
                    // never reaches it as a start. `stream.left` releases the
                    // viewer and lets the waiting publication start.
                    if let Err(end) = live
                        .on_screen_signal(
                            &event.kind,
                            &event.request_id,
                            &event.data,
                            now,
                            viewer,
                        )
                        .await
                    {
                        return end;
                    }
                    if event.kind == "stream.snapshot" {
                        // The fresh list is authoritative for this socket and
                        // must arrive before a recovered publication starts.
                        live.note_snapshot();
                    }
                    // `stream.watching` refreshes the publication in the list.
                    live.updates
                        .push(VoiceUpdate::Streams(viewer.catalog().list().to_vec()));
                    if let Err(end) = live.recover_publish(viewer).await {
                        return end;
                    }
                    continue;
                }
                // A close/refusal of a socket authorized with the rotated-away
                // token belongs to the rotation, not to the new session.
                if matches!(
                    event.kind.as_str(),
                    "voice.webrtc_closed" | "voice.left" | "error"
                ) && live.stale_session(client)
                {
                    return SessionEnd::Recover;
                }
                let actions = controller.on_signal(&event.kind, &event.data);
                match live.execute(actions).await {
                    Ok(true) => {}
                    Ok(false) => return SessionEnd::Recover,
                    Err(end) => return end,
                }
                live.media.set_joined(controller.joined);
                live.media
                    .set_server_allowed(!controller.server_muted && !controller.controls.deafened);
                // A changed connection id or a lost join invalidates a
                // preparation that was bound to the old session.
                let connection = live.set_connection(controller.resume_connection_id.clone());
                if let Err(end) = live.apply_screen(connection, viewer).await {
                    return end;
                }
                let joined = live.set_joined(controller.joined);
                if let Err(end) = live.apply_screen(joined, viewer).await {
                    return end;
                }
                // The viewer follows the confirmed voice state, not the socket:
                // a fresh join re-watches the selected publication.
                let outcomes = viewer.set_joined(controller.joined, Instant::now());
                if let Err(end) = live.apply(outcomes, viewer).await {
                    return end;
                }
                if let Err(end) = live.recover_publish(viewer).await {
                    return end;
                }
                live.updates
                    .push(VoiceUpdate::Participants(controller.participants.clone()));
                live.updates.push(VoiceUpdate::SelfConnection(
                    controller.resume_connection_id.clone(),
                ));
                live.updates
                    .push(VoiceUpdate::Speaking(controller.speaking.clone()));
                live.updates
                    .push(VoiceUpdate::Tracks(controller.connections_with_audio()));
            }
            _ = tick.tick() => {
                live.viewport = requests.viewport();
                let now = Instant::now();
                let outcomes = viewer.tick(now);
                if let Err(end) = live.apply(outcomes, viewer).await {
                    return end;
                }
                if let Err(end) = live.tick_screen(now, viewer).await {
                    return end;
                }
                match live.drain_voice_media(controller).await {
                    Ok(true) => {}
                    Ok(false) => return SessionEnd::Recover,
                    Err(end) => return end,
                }
                match live.drain_viewer(viewer).await {
                    Ok(true) => {}
                    Ok(false) => return SessionEnd::Recover,
                    Err(end) => return end,
                }
            }
        }
    }
}

pub(super) fn run(
    client: voxhold_api::Client,
    server: i64,
    channel: i64,
    controls: Controls,
    self_muted: bool,
    requests: Arc<Requests>,
    updates: Arc<Updates>,
    frame: Arc<Mutex<Option<StreamFrame>>>,
    capture: Arc<CaptureGate>,
    screen_preview: Arc<crate::screen_capture::PreviewStore>,
) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        updates.push(VoiceUpdate::Ended(Some(
            "Не удалось запустить голосовое соединение".into(),
        )));
        return;
    };
    let mut controller = VoiceController::new(server, channel, controls, self_muted);
    let mut viewer = Viewer::new(server, channel);
    let mut retries = 0;
    loop {
        if requests.leave.swap(false, Ordering::SeqCst) {
            break;
        }
        updates.push(VoiceUpdate::Connecting);
        let started = Instant::now();
        let end = runtime.block_on(run_session(
            &client,
            &requests,
            &mut controller,
            &mut viewer,
            updates.clone(),
            frame.clone(),
            capture.clone(),
            screen_preview.clone(),
        ));
        controller.joined = false;
        // A reconnect never resumes the old capture, but a confirmed
        // publication is an intent that outlives this socket. The intent is
        // captured by `Live` while the publisher still describes the live
        // attempt; an explicit Stop/Leave already bumped the epoch and left
        // nothing to capture.
        let recover = matches!(&end, SessionEnd::Recover);
        let recovering = if recover {
            requests
                .publish_recovery
                .take(requests.cancellation_epoch())
        } else {
            requests.publish_recovery.clear();
            None
        };
        screen_preview.clear();
        updates.push(VoiceUpdate::ScreenShare(match &recovering {
            Some(intent) => intent.sharing.clone(),
            None => crate::publisher::Sharing::default(),
        }));
        let outcomes = viewer.transport_ended(Instant::now());
        for outcome in outcomes {
            match outcome {
                ViewerOutcome::Status { selection, text } => {
                    updates.push(VoiceUpdate::StreamStatus {
                        selection,
                        status: text,
                    });
                }
                ViewerOutcome::Connected {
                    selection,
                    connected,
                } => {
                    updates.push(VoiceUpdate::StreamConnected {
                        selection,
                        connected,
                    });
                }
                ViewerOutcome::Close => {
                    if let Ok(mut frame) = frame.lock() {
                        frame.take();
                    }
                }
                _ => {}
            }
        }
        updates.push(VoiceUpdate::Streams(Vec::new()));
        if let Ok(mut frame) = frame.lock() {
            frame.take();
        }
        match end {
            SessionEnd::Leave => break,
            SessionEnd::Error(message) => {
                updates.push(VoiceUpdate::Ended(Some(message)));
                return;
            }
            SessionEnd::Recover => {}
        }
        if recover {
            if let Some(intent) = recovering {
                // Keep the intent across the wait and the next socket. A Stop
                // or Leave during either period bumps the epoch and clears it.
                requests
                    .publish_recovery
                    .store(intent, requests.cancellation_epoch());
            }
        }
        if started.elapsed() > LONG_SESSION {
            retries = 0;
        }
        retries += 1;
        if retries > MAX_RECONNECTS {
            updates.push(VoiceUpdate::Ended(Some(
                "Не удалось восстановить голосовое соединение".into(),
            )));
            return;
        }
        updates.push(VoiceUpdate::Reconnecting);
        // The old socket ended. A rotation that is still waiting for its
        // refresh answer owns the next authorization; the ordinary delay only
        // applies when no rotation is in flight.
        requests.rotation_socket_closed();
        let ordinary = Instant::now() + RECONNECT_DELAY;
        let mut aborted = false;
        loop {
            while let Some(pending) = requests.take() {
                match pending {
                    Pending::Leave => {
                        aborted = true;
                        break;
                    }
                    Pending::Controls(controls, self_muted) => {
                        // A control snapshot queued before `SelectInput` must
                        // not replace the device for the next connection.
                        let input = controller.controls.input.clone();
                        controller.controls = controls;
                        controller.controls.input = input;
                        controller.self_muted = self_muted;
                    }
                    Pending::Output(output) => {
                        controller.controls.output = output;
                    }
                    Pending::Input(request) => {
                        controller.controls.input = request.input;
                    }
                    Pending::LocalMute(connection, muted) => {
                        controller.set_local_mute(&connection, muted);
                    }
                    Pending::Stream(StreamCommand::Watch {
                        selection,
                        publication,
                    }) => {
                        viewer.select_intent(publication, selection);
                    }
                    Pending::Stream(StreamCommand::Close { selection }) => {
                        let _ = viewer.leave_selection(selection, Instant::now());
                    }
                    Pending::StreamMute(selection, muted) => {
                        let _ = viewer.set_muted(selection, muted);
                    }
                    Pending::Screen(ScreenCommand::Stop) => {
                        // The user stopped the broadcast while the call is
                        // reconnecting. The intent is gone before the next
                        // join confirmation can act on it, and the room must
                        // not keep painting the recovery card.
                        requests.cancel_publish_recovery();
                        updates.push(VoiceUpdate::ScreenShare(
                            crate::publisher::Sharing::default(),
                        ));
                    }
                    Pending::Screen(_) | Pending::ScreenSources(_) | Pending::ScreenAccess(_) => {
                        // No session, no publisher and no capture: a picker
                        // action of the previous attempt is dropped. The UI
                        // cannot reach these states without a live call.
                    }
                    Pending::Power(_) => {
                        // No engine exists between attempts: the next session
                        // opens its sink from the controls snapshot.
                    }
                }
            }
            if aborted {
                break;
            }
            let now = Instant::now();
            if requests.rotation_should_reconnect(now, ordinary) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if aborted {
            break;
        }
        // The new socket owns the link now. A late refresh answer or an old
        // close can no longer trigger another authorization.
        requests.rotation_reconnected();
    }
    let _ = frame.lock().map(|mut frame| frame.take());
    updates.push(VoiceUpdate::Ended(None));
}
