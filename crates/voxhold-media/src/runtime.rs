//! Background voice connection. UI callers send controls and consume snapshots.
use crate::{
    Command, Controls, IceServer, Media,
    controller::{Action, Participant, Publication, VoiceController},
    stream::{StreamViewer, ViewerUpdate},
};
use std::{sync::{mpsc as std_mpsc, Arc, atomic::AtomicU64}, thread, time::{Duration, Instant}};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub enum VoiceUpdate {
    Connecting,
    Connected,
    Participants(Vec<Participant>),
    Speaking(std::collections::HashMap<String, bool>),
    Error(String),
    Left,
    Streams(Vec<Publication>),
    StreamStatus(String),
}
#[derive(Debug)]
enum Request {
    Controls(Controls, bool),
    LocalMute(String, bool),
    Leave,
    Watch(Publication),
    LeaveStream,
    StreamMute(bool),
}
pub struct StreamFrame { pub width: u32, pub height: u32, pub rgba: Vec<u8> }
pub struct VoiceHandle {
    requests: mpsc::UnboundedSender<Request>,
    updates: std_mpsc::Receiver<VoiceUpdate>,
    worker: Option<thread::JoinHandle<()>>,
    frame: std::sync::Arc<std::sync::Mutex<Option<StreamFrame>>>,
}
impl VoiceHandle {
    pub fn join(
        client: voxhold_api::Client,
        server: i64,
        channel: i64,
        controls: Controls,
        self_muted: bool,
    ) -> Result<Self, String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (updates, output) = std_mpsc::channel();
        let frame=std::sync::Arc::new(std::sync::Mutex::new(None));
        let worker_frame=frame.clone();
        let worker = thread::Builder::new()
            .name("voxhold-voice-session".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    let _ = updates.send(VoiceUpdate::Error(
                        "Не удалось запустить голосовое соединение".into(),
                    ));
                    return;
                };
                runtime.block_on(run(
                    client, server, channel, controls, self_muted, rx, updates, worker_frame,
                ));
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            requests: tx,
            updates: output,
            worker: Some(worker),
            frame,
        })
    }
    pub fn controls(&self, controls: Controls, self_muted: bool) {
        let _ = self.requests.send(Request::Controls(controls, self_muted));
    }
    pub fn local_mute(&self, connection: String, muted: bool) {
        let _ = self.requests.send(Request::LocalMute(connection, muted));
    }
    pub fn take_frame(&self) -> Option<StreamFrame> { self.frame.lock().ok()?.take() }
    pub fn try_update(&self) -> Option<VoiceUpdate> {
        self.updates.try_recv().ok()
    }
    pub fn watch_stream(&self, publication: Publication) { if let Ok(mut slot)=self.frame.lock(){slot.take();} let _=self.requests.send(Request::Watch(publication)); }
    pub fn leave_stream(&self) { if let Ok(mut slot)=self.frame.lock(){slot.take();} let _=self.requests.send(Request::LeaveStream); }
    pub fn stream_mute(&self, muted: bool) { let _=self.requests.send(Request::StreamMute(muted)); }
    pub fn leave(&self) {
        let _ = self.requests.send(Request::Leave);
    }
}
impl Drop for VoiceHandle {
    fn drop(&mut self) {
        self.leave();
        self.worker.take();
    }
}
async fn execute(
    actions: Vec<Action>,
    media: &Media,
    socket: &mut voxhold_api::VoiceSocket,
) -> Result<bool, String> {
    for action in actions {
        match action {
            Action::Media(command) => media
                .command(command)
                .map_err(|e| format!("Ошибка медиадвижка: {e:?}"))?,
            Action::Send(s) => socket
                .send(s.kind, s.data, s.request_id.as_deref())
                .await
                .map_err(|e| e.to_string())?,
            Action::Reconnect => return Ok(false),
            Action::Closed(e) => return Err(e.into()),
        }
    }
    Ok(true)
}
#[derive(Clone)]
struct StreamIntent { publication: Publication, muted: bool }
async fn waiting<F: std::future::Future>(
    future: F,
    requests: &mut mpsc::UnboundedReceiver<Request>,
    controller: &mut VoiceController,
    intent: &mut Option<StreamIntent>,
) -> Option<F::Output> {
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return Some(result),
            request = requests.recv() => match request {
                Some(Request::Controls(c,m)) => { controller.update_controls(c,m); },
                Some(Request::LocalMute(id,m)) => { controller.set_local_mute(&id,m); },
                Some(Request::Watch(publication)) => { *intent=Some(StreamIntent {publication,muted:false}); },
                Some(Request::LeaveStream) => { *intent=None; },
                Some(Request::StreamMute(muted)) => { if let Some(intent)=intent {intent.muted=muted;} },
            _ => return None,
            }
        }
    }
}
async fn run(
    client: voxhold_api::Client,
    server: i64,
    channel: i64,
    controls: Controls,
    self_muted: bool,
    mut requests: mpsc::UnboundedReceiver<Request>,
    updates: std_mpsc::Sender<VoiceUpdate>,
    frame: std::sync::Arc<std::sync::Mutex<Option<StreamFrame>>>,
) {
    let mut controller = VoiceController::new(server, channel, controls, self_muted);
    let mut streams: Vec<Publication> = Vec::new();
    let mut intent: Option<StreamIntent> = None;
    let sequence=Arc::new(AtomicU64::new(0));
    'outer: loop {
        let _ = updates.send(VoiceUpdate::Connecting);
        let config = match waiting(client.webrtc_config(), &mut requests, &mut controller, &mut intent).await {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                let _ = updates.send(VoiceUpdate::Error(e.to_string()));
                break;
            }
            None => break,
        };
        let mut socket =
            match waiting(client.open_voice_socket(), &mut requests, &mut controller, &mut intent).await {
                Some(Ok(v)) => v,
                Some(Err(e)) => {
                    let _ = updates.send(VoiceUpdate::Error(e.to_string()));
                    break;
                }
                None => break,
            };
        let media = match Media::new() {
            Ok(v) => v,
            Err(e) => {
                let _ = updates.send(VoiceUpdate::Error(format!("{e:?}")));
                break;
            }
        };
        let servers: Vec<IceServer> = config
            .ice_servers
            .into_iter()
            .map(|s| IceServer {
                urls: s.urls,
                username: s.username,
                credential: s.credential,
            })
            .collect();
        if let Err(e) = media.command(Command::Start {
            ice_servers: servers.clone(),
            ice_transport_policy: config.ice_transport_policy.clone(),
            controls: {
                let mut c = controller.controls.clone();
                c.muted |= controller.server_muted;
                c
            },
        }) {
            let _ = updates.send(VoiceUpdate::Error(format!("{e:?}")));
            break;
        }
        let mut viewer: Option<StreamViewer> = None;
        let mut pending_leave: Option<(String, Instant)> = None;
        let mut viewer_deadline: Option<Instant> = None;
        let mut viewer_retries: u8 = 0;
        let mut tick = tokio::time::interval(Duration::from_millis(25));
        let mut reconnect = false;
        loop {
            tokio::select! {
                request=requests.recv()=>{
                    let actions=match request {
                        Some(Request::Controls(c,m))=>controller.update_controls(c,m),
                        Some(Request::LocalMute(id,m))=>controller.set_local_mute(&id,m),
                        Some(Request::Watch(publication))=>{
                            if publication.server_id != server || publication.channel_id != channel { continue; }
                            intent=Some(StreamIntent {publication,muted:false}); viewer_retries=0; viewer_deadline=None;
                            if let Some(mut old)=viewer.take() {
                                let signal=old.leave(); let id=signal.request_id.clone().unwrap_or_default();
                                if socket.send(signal.kind,signal.data,Some(&id)).await.is_err(){reconnect=true;break;}
                                pending_leave=Some((id,Instant::now()+Duration::from_secs(5)));
                            }
                            if let Ok(mut slot)=frame.lock(){slot.take();}
                            let _=updates.send(VoiceUpdate::StreamStatus("Подключение…".into())); continue;
                        },
                        Some(Request::LeaveStream)=>{
                            intent=None; viewer_deadline=None;
                            if let Some(mut old)=viewer.take(){let signal=old.leave();let id=signal.request_id.clone().unwrap_or_default();if socket.send(signal.kind,signal.data,Some(&id)).await.is_err(){reconnect=true;break;}pending_leave=Some((id,Instant::now()+Duration::from_secs(5)));}
                            if let Ok(mut slot)=frame.lock(){slot.take();}let _=updates.send(VoiceUpdate::StreamStatus(String::new()));continue;
                        },
                        Some(Request::StreamMute(muted))=>{if let Some(intent)=&mut intent {intent.muted=muted;} if let Some(viewer)=&mut viewer {let _=viewer.set_muted(muted);}continue;},
                        _=>{if let Some(mut old)=viewer.take(){let signal=old.leave();let _=socket.send(signal.kind,signal.data,signal.request_id.as_deref()).await;}let _=socket.send("voice.leave",serde_json::json!({}),None).await; let _=socket.close().await; break 'outer}
                    };
                    match execute(actions,&media,&mut socket).await {Ok(true)=>{},Ok(false)=>{reconnect=true;break},Err(e)=>{let _=updates.send(VoiceUpdate::Error(e));break 'outer}}
                },
                event=socket.read()=>{
                    let event=match event {Ok(e)=>e,Err(_)=>{reconnect=true;break}};
                    match event.kind.as_str() {
                        "stream.snapshot"=>{streams=serde_json::from_value::<Vec<Publication>>(event.data["streams"].clone()).unwrap_or_default().into_iter().filter(|p|p.server_id==server && p.channel_id==channel && !p.publisher_connection_id.is_empty()).collect();
                            if !controller.joined {if let Some(selected)=&mut intent {let replacement=streams.iter().find(|p|p.publisher_connection_id==selected.publication.publisher_connection_id).or_else(||streams.iter().find(|p|p.publisher_user_id==selected.publication.publisher_user_id));if let Some(p)=replacement {selected.publication=p.clone();}}}
                            let _=updates.send(VoiceUpdate::Streams(streams.clone()));},
                        "stream.started"|"stream.updated"=>{if let Ok(p)=serde_json::from_value::<Publication>(event.data.clone()){if p.server_id==server && p.channel_id==channel && !p.publisher_connection_id.is_empty(){streams.retain(|old|old.publisher_connection_id!=p.publisher_connection_id);streams.push(p);let _=updates.send(VoiceUpdate::Streams(streams.clone()));}}},
                        "stream.stopped"=>{let id=event.data["publisher_connection_id"].as_str().unwrap_or_default();streams.retain(|p|p.publisher_connection_id!=id);let _=updates.send(VoiceUpdate::Streams(streams.clone()));}, _=>{}
                    }
                    if event.kind.starts_with("stream.") || (event.kind=="error" && event.request_id.starts_with("native-stream-")) {
                        if event.kind=="stream.left" && pending_leave.as_ref().is_some_and(|(id,_)|*id==event.request_id) {pending_leave=None;continue;}
                        if event.kind=="stream.left" && event.request_id.is_empty() && viewer.is_some() {viewer=None;intent=None;viewer_deadline=None;let _=updates.send(VoiceUpdate::StreamStatus("Просмотр трансляции завершён сервером".into()));continue;}
                        if event.kind=="stream.webrtc_closed" && viewer.is_some() {
                            let reason=event.data["reason"].as_str().unwrap_or_default().to_lowercase();
                            let final_close=["permission","access","stopped","replaced","e2ee"].iter().any(|s|reason.contains(s));
                            if final_close || viewer_retries>=2 {viewer=None;intent=None;viewer_deadline=None;let _=updates.send(VoiceUpdate::StreamStatus("Трансляция завершена".into()));}
                            else if let Some(mut old)=viewer.take() {viewer_retries+=1;viewer_deadline=None;let signal=old.leave();let id=signal.request_id.clone().unwrap_or_default();if socket.send(signal.kind,signal.data,Some(&id)).await.is_err(){reconnect=true;break;}pending_leave=Some((id,Instant::now()+Duration::from_secs(5)));}
                            continue;
                        }
                        let failure=viewer.as_mut().and_then(|current|current.on_signal(&event.kind,&event.request_id,&event.data).err());
                        if let Some(e)=failure {intent=None;viewer_deadline=None;if let Some(mut old)=viewer.take(){let signal=old.leave();let id=signal.request_id.clone().unwrap_or_default();if socket.send(signal.kind,signal.data,Some(&id)).await.is_err(){reconnect=true;break;}pending_leave=Some((id,Instant::now()+Duration::from_secs(5)));}let _=updates.send(VoiceUpdate::StreamStatus(e));} continue;
                    }
                    let actions=controller.on_signal(&event.kind,&event.data); let _=updates.send(VoiceUpdate::Participants(controller.participants.clone()));
                    match execute(actions,&media,&mut socket).await {Ok(true)=>{},Ok(false)=>{reconnect=true;break},Err(e)=>{let _=updates.send(VoiceUpdate::Error(e));break 'outer}}
                },
                _=tick.tick()=>{
                    while let Some(event)=media.poll(Duration::ZERO) {if matches!(event,crate::Event::State{state:"connected"}) {let _=updates.send(VoiceUpdate::Connected);} let actions=controller.media_event(event); let _=updates.send(VoiceUpdate::Speaking(controller.speaking.clone())); match execute(actions,&media,&mut socket).await {Ok(true)=>{},Ok(false)=>{reconnect=true;break},Err(e)=>{let _=updates.send(VoiceUpdate::Error(e));break 'outer}}}
                    if let Some(current)=&mut viewer {match current.poll(960,540) {
                        Some(ViewerUpdate::Frame)=>{viewer_deadline=None;if let Ok(mut slot)=frame.lock() {*slot=Some(StreamFrame {width:current.frame.width(),height:current.frame.height(),rgba:current.frame.rgba.clone()});}},
                        Some(ViewerUpdate::Status(s))=>{let _=updates.send(VoiceUpdate::StreamStatus(s.into()));},
                        Some(ViewerUpdate::Error(e))=>{let _=updates.send(VoiceUpdate::StreamStatus(e));intent=None;viewer_deadline=None;if let Some(mut old)=viewer.take(){let signal=old.leave();let id=signal.request_id.clone().unwrap_or_default();if socket.send(signal.kind,signal.data,Some(&id)).await.is_err(){reconnect=true;}else {pending_leave=Some((id,Instant::now()+Duration::from_secs(5)));}}},
                        Some(ViewerUpdate::Signal(signal))=>{if signal.kind=="stream.p2p_restart" || signal.kind=="stream.recovery_request" {viewer_deadline=Some(Instant::now()+Duration::from_secs(15));} if socket.send(signal.kind,signal.data,signal.request_id.as_deref()).await.is_err(){reconnect=true;}},None=>{}
                    }}
                    if reconnect {break}
                    if pending_leave.as_ref().is_some_and(|(_,deadline)|Instant::now()>=*deadline) {
                        pending_leave=None; if intent.is_some(){intent=None;let _=updates.send(VoiceUpdate::StreamStatus("Не удалось завершить предыдущий просмотр".into()));}
                    }
                    if viewer_deadline.is_some_and(|deadline|Instant::now()>=deadline) {
                        viewer_deadline=None;
                        if viewer_retries>=2 {intent=None;if let Some(mut old)=viewer.take(){let signal=old.leave();let id=signal.request_id.clone().unwrap_or_default();if socket.send(signal.kind,signal.data,Some(&id)).await.is_err(){reconnect=true;break;}pending_leave=Some((id,Instant::now()+Duration::from_secs(5)));}let _=updates.send(VoiceUpdate::StreamStatus("Не удалось восстановить трансляцию".into()));}
                        else if let Some(mut old)=viewer.take(){viewer_retries+=1;let signal=old.leave();let id=signal.request_id.clone().unwrap_or_default();if socket.send(signal.kind,signal.data,Some(&id)).await.is_err(){reconnect=true;break;}pending_leave=Some((id,Instant::now()+Duration::from_secs(5)));}
                    }
                    if controller.joined && viewer.is_none() && pending_leave.is_none() {
                        if let Some(selected)=intent.clone() {
                            match StreamViewer::start(selected.publication,servers.clone(),config.ice_transport_policy.clone(),controller.controls.output.clone(),selected.muted,sequence.clone()) {
                                Ok((new,signal))=>{if socket.send(signal.kind,signal.data,signal.request_id.as_deref()).await.is_err(){reconnect=true;break;} viewer=Some(new);viewer_deadline=Some(Instant::now()+Duration::from_secs(15));let _=updates.send(VoiceUpdate::StreamStatus("Подключение…".into()));},
                                Err(e)=>{intent=None;let _=updates.send(VoiceUpdate::StreamStatus(e));}
                            }
                        }
                    }
                },
            }
        }
        controller.joined = false;
        if let Ok(mut slot)=frame.lock(){slot.take();}
        if intent.is_some(){let _=updates.send(VoiceUpdate::StreamStatus("Восстанавливаем трансляцию…".into()));}
        let _ = media.command(Command::Stop);
        drop(media);
        if !reconnect {
            break;
        }
        if waiting(
            tokio::time::sleep(Duration::from_secs(3)),
            &mut requests,
            &mut controller,
            &mut intent,
        )
        .await
        .is_none()
        {
            break;
        }
    }
    let _ = updates.send(VoiceUpdate::Left);
}

