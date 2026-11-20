//! Server call recordings, W01. The wire shapes follow
//! `voxhold-backend/internal/recordings`: `created_at`/`expires_at` are Unix
//! seconds, `updated_at` is a monotonic millisecond revision, `duration_ms` is
//! milliseconds and the stored size is 64-bit.
//!
//! The list and metadata are ordinary JSON calls. The preview is a private
//! JPEG, the ticket is a relative signed route on the same origin, and the
//! ticket download must stream outside the eight-second JSON timeout without a
//! user Bearer header. A foreign or absolute ticket never leaves the origin.

use super::*;
use serde::{Deserialize, Serialize};

/// The statuses reported by the server.
pub const RECORDING_STARTED: &str = "recording";
pub const RECORDING_UPLOADING: &str = "uploading";
pub const RECORDING_READY: &str = "ready";
pub const RECORDING_LOCAL_ONLY: &str = "local_only";
pub const RECORDING_FAILED: &str = "failed";
pub const RECORDING_DELETED: &str = "deleted";

/// Go `MaxPreviewUpload`, with headroom so a declared length is never trusted.
const PREVIEW_WIRE_LIMIT: usize = 128 << 10;
/// Go `DownloadRecording` limit.
const DOWNLOAD_LIMIT: u64 = 2 << 30;
const TICKET_PREFIX: &str = "/api/v1/recording-downloads/";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Recording {
    pub id: String,
    pub server_id: i64,
    pub channel_id: i64,
    pub owner_user_id: i64,
    pub owner_username: String,
    pub status: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub duration_ms: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub preview_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingList {
    pub recordings: Vec<Recording>,
    pub quota_bytes: i64,
    pub max_file_bytes: i64,
    pub retention_seconds: i64,
}

impl Recording {
    /// The server chooses the stored container; the download name follows it.
    pub fn extension(&self) -> &'static str {
        if self.mime_type == "video/mp4" {
            ".mp4"
        } else {
            ".webm"
        }
    }

    /// The same name the server would offer: UTC timestamp plus the first
    /// eight characters of the recording id.
    pub fn filename(&self) -> String {
        let stamp = utc_stamp(self.created_at);
        let prefix = self.id.get(..8).unwrap_or(&self.id);
        format!("voxhold-{stamp}-{prefix}{}", self.extension())
    }
}

fn valid_recording_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_ticket(ticket: &str) -> bool {
    let Some(rest) = ticket.strip_prefix(TICKET_PREFIX) else {
        return false;
    };
    if rest.is_empty() || rest.contains("//") || rest.contains(['/', '?', '#', '\\']) {
        return false;
    }
    let mut parts = rest.split('.');
    let (Some(payload), Some(signature), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !payload.is_empty()
        && !signature.is_empty()
        && payload
            .bytes()
            .chain(signature.bytes())
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Client {
    pub async fn list_recordings(&self, server_id: i64) -> Result<RecordingList> {
        if server_id <= 0 {
            return Err(Error::InvalidResponse("Некорректный сервер записей."));
        }
        self.request(
            Method::GET,
            &format!("/api/v1/servers/{server_id}/recordings"),
            None,
        )
        .await
    }

    pub async fn get_recording(&self, id: &str) -> Result<Recording> {
        if !valid_recording_id(id) {
            return Err(Error::InvalidResponse("Некорректная запись."));
        }
        self.request(Method::GET, &format!("/api/v1/recordings/{id}"), None)
            .await
    }

    pub async fn delete_recording(&self, id: &str) -> Result<Recording> {
        if !valid_recording_id(id) {
            return Err(Error::InvalidResponse("Некорректная запись."));
        }
        self.request(Method::DELETE, &format!("/api/v1/recordings/{id}"), None)
            .await
    }

    /// The private 320x180 JPEG. The version is part of the request so a
    /// replaced preview is never served from a stale cache.
    pub async fn recording_preview(&self, id: &str, version: &str) -> Result<image::DynamicImage> {
        if !valid_recording_id(id) || version.is_empty() || version.len() > 128 {
            return Err(Error::InvalidResponse("Некорректное превью записи."));
        }
        let mut url = self.url(&format!("/api/v1/recordings/{id}/preview"))?;
        url.query_pairs_mut().append_pair("v", version);
        let mut request = self.http.get(url);
        let token = self.auth.token();
        if !token.is_empty() {
            request = request.bearer_auth(token.as_ref());
        }
        let response = request.send().await.map_err(|_| Error::Network)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(self.refused(response.status().as_u16(), &token));
        }
        let media = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !media.starts_with("image/jpeg") {
            return Err(Error::InvalidResponse("invalid recording preview type"));
        }
        let bytes = read_wire_capped(response, PREVIEW_WIRE_LIMIT).await?;
        decode_preview(&bytes)
    }

    /// Exchange an authenticated request for the signed relative route. The
    /// token in the query must never leave this origin.
    pub async fn recording_download_ticket(&self, id: &str) -> Result<String> {
        if !valid_recording_id(id) {
            return Err(Error::InvalidResponse("Некорректная запись."));
        }
        let value: Value = self
            .request(
                Method::POST,
                &format!("/api/v1/recordings/{id}/download"),
                None,
            )
            .await?;
        let ticket = value["url"]
            .as_str()
            .filter(|ticket| valid_ticket(ticket))
            .ok_or(Error::InvalidResponse("invalid recording download url"))?;
        Ok(ticket.to_owned())
    }

    /// Stream a ready record through its ticket. No user Bearer is sent, the
    /// shared client still refuses redirects, and the caller owns the sink
    /// (usually a `.part` file on a worker thread).
    pub async fn download_recording_to(
        &self,
        ticket: &str,
        expect: i64,
        sink: &mut dyn FnMut(&[u8]) -> std::result::Result<(), String>,
    ) -> Result<u64> {
        if !valid_ticket(ticket) {
            return Err(Error::InvalidResponse("invalid recording download"));
        }
        if expect < 0 || expect as u64 > DOWNLOAD_LIMIT {
            return Err(Error::Local(
                "Запись превышает допустимый размер скачивания.".into(),
            ));
        }
        let seconds = (expect.max(0) as f64 / (512.0 * 1024.0) + 60.0).max(120.0);
        let response = self
            .http
            .get(self.url(ticket)?)
            .timeout(Duration::from_secs_f64(seconds))
            .send()
            .await
            .map_err(|_| Error::Network)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(Error::Http(response.status().as_u16()));
        }
        if let (Some(length), true) = (response.content_length(), expect > 0) {
            if length != expect as u64 {
                return Err(Error::Local(
                    "Размер записи изменился. Обновите список.".into(),
                ));
            }
        }
        let mut response = response;
        let mut written = 0u64;
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
            written = written.saturating_add(chunk.len() as u64);
            if written > DOWNLOAD_LIMIT {
                return Err(Error::Local(
                    "Запись превышает допустимый размер скачивания.".into(),
                ));
            }
            sink(&chunk).map_err(Error::Local)?;
        }
        if expect > 0 && written != expect as u64 {
            return Err(Error::Local(format!(
                "Запись весит {written} байт вместо {expect}."
            )));
        }
        Ok(written)
    }
}

async fn read_wire_capped(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Network)? {
        if out.len().saturating_add(chunk.len()) > max {
            return Err(Error::InvalidResponse("invalid recording preview size"));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

fn decode_preview(bytes: &[u8]) -> Result<image::DynamicImage> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| Error::InvalidResponse("invalid recording preview"))?;
    let format = reader
        .format()
        .ok_or(Error::InvalidResponse("invalid recording preview"))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|_| Error::InvalidResponse("invalid recording preview"))?;
    if format != image::ImageFormat::Jpeg
        || width == 0
        || height == 0
        || width > 1280
        || height > 720
        || u64::from(width) * u64::from(height) > 4_000_000
    {
        return Err(Error::InvalidResponse(
            "invalid recording preview dimensions",
        ));
    }
    image::load_from_memory_with_format(bytes, format)
        .map_err(|_| Error::InvalidResponse("invalid recording preview"))
}

/// `YYYYMMDD-HHMMSS` in UTC without pulling a date library into the client.
fn utc_stamp(seconds: i64) -> String {
    let (year, month, day, hour, minute, second) = utc_parts(seconds);
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

fn utc_parts(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days, valid for the whole i64 range we use.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (
        year,
        month,
        day,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_ids_are_checked_before_io() {
        for id in [
            "",
            "short",
            &"z".repeat(32),
            &"A".repeat(32),
            "0123456789abcdef0123456789abcde/",
        ] {
            assert!(!valid_recording_id(id), "{id}");
        }
        assert!(valid_recording_id("0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn tickets_stay_on_the_recording_route() {
        for ticket in [
            "/api/v1/recording-downloads/YWJj.ZGVm",
            "/api/v1/recording-downloads/a_b-c.d_e-f",
        ] {
            assert!(valid_ticket(ticket), "{ticket}");
        }
        for ticket in [
            "https://evil.example/api/v1/recording-downloads/a.b",
            "//evil.example/api/v1/recording-downloads/a.b",
            "/api/v1/recording-downloads/../../admin",
            "/api/v1/recording-downloads/a/b",
            "/api/v1/recording-downloads/a.b?x=1",
            "/api/v1/recording-downloads/a.b#x",
            "/api/v1/attachments/x/content",
            "/api/v1/recording-downloads/a",
            "",
        ] {
            assert!(!valid_ticket(ticket), "{ticket}");
        }
    }

    #[test]
    fn filename_uses_utc_and_the_container() {
        let mut record = Recording {
            id: "0123456789abcdef0123456789abcdef".into(),
            mime_type: "video/mp4".into(),
            created_at: 1_700_000_000,
            ..Recording::default()
        };
        assert_eq!(record.filename(), "voxhold-20231114-221320-01234567.mp4");
        record.mime_type = "video/webm".into();
        assert_eq!(record.extension(), ".webm");
    }

    #[test]
    fn invalid_preview_bytes_are_rejected() {
        assert!(decode_preview(b"<html>").is_err());
        // A JPEG header with a wrong container size never reaches decode.
        let big = image::RgbaImage::new(1300, 10);
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(big)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert!(decode_preview(&bytes).is_err());
    }

    #[test]
    fn utc_conversion_matches_known_dates() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1_700_000_000), "20231114-221320");
        assert_eq!(utc_stamp(951_782_400), "20000229-000000");
    }
}
