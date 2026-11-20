//! Server reasons of a screen publication, kept beside the state machine so a
//! reviewer sees the same mapping the on-air error uses. Pure functions: no
//! socket, no engine.

use serde_json::Value;

/// Media-session reasons the backend emits while tearing down a publication
/// that the client already stopped. They are not a permission or voice loss.
pub(super) fn stale_media_close(reason: &str) -> bool {
    matches!(
        reason,
        "stream WebRTC connection closed"
            | "stream signaling connection was lost"
            | "stream WebRTC recovery exhausted"
    )
}

pub(super) fn server_error(data: &Value) -> String {
    let message = data["message"].as_str().unwrap_or_default();
    match data["code"].as_str().unwrap_or_default() {
        "forbidden" => "Нет права запускать трансляцию.".into(),
        "invalid_payload" => "Сервер отклонил параметры трансляции.".into(),
        "internal_error" => "Сервер не смог запустить трансляцию.".into(),
        "invalid_state" if message.contains("limit reached") => {
            "В канале уже 8 трансляций. Дождитесь завершения одной из них.".into()
        }
        "invalid_state" if message.contains("same voice channel") => {
            "Сначала подключитесь к голосовому каналу.".into()
        }
        "invalid_state" if message.contains("already active") => "Трансляция уже запущена.".into(),
        _ => "Сервер отклонил запуск трансляции.".into(),
    }
}

pub(super) fn reason_message(reason: &str) -> &'static str {
    let reason = reason.to_lowercase();
    if reason.contains("permission") || reason.contains("access") {
        "Доступ к трансляции отозван."
    } else if reason.contains("voice") {
        "Голосовое соединение завершилось, трансляция остановлена."
    } else if reason.contains("bitrate") {
        "Превышен битрейт трансляции."
    } else if reason.contains("rendition") {
        "Сервер не принял параметры трансляции."
    } else {
        "Сервер остановил трансляцию."
    }
}
