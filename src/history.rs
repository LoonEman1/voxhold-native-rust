//! History row model ported from Go `internal/workspace/widgets.go`.
//!
//! Only pure derivations live here: grouping, date separators, the unread
//! boundary and the fixed geometry of attachments/quotes. Rendering stays in
//! `main.rs` next to the other GPUI surfaces.

use voxhold_api::Message;

/// Go `History.measure`: the usable text width is `history_width - 88`.
pub const HISTORY_TEXT_INSET: f32 = 88.0;
/// Go `quoteHeight`.
pub const QUOTE_HEIGHT: f32 = 75.0;
/// Go `reactionPitch`.
pub const REACTION_PITCH: f32 = 62.0;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowMeta {
    /// The row belongs to the previous author header.
    pub grouped: bool,
    /// A date divider is drawn above this row.
    pub date: Option<String>,
    /// The "Новые сообщения" divider is drawn above this row.
    pub unread: bool,
}

pub fn same_day(a: i64, b: i64) -> bool {
    let (ay, am, ad, _, _) = local_parts(a);
    let (by, bm, bd, _, _) = local_parts(b);
    (ay, am, ad) == (by, bm, bd)
}

/// Go groups consecutive messages from the same author when they are less than
/// five minutes apart, on the same day, not in a news channel and none of the
/// two rows is a reply or a forward.
pub fn can_group(previous: &Message, current: &Message, channel_kind: &str) -> bool {
    previous.id != 0
        && current.reply.is_none()
        && current.forward.is_none()
        && previous.reply.is_none()
        && previous.forward.is_none()
        && previous.author.user_id == current.author.user_id
        && current.author.user_id > 0
        && current.created_at - previous.created_at < 300
        && channel_kind != "news"
        && same_day(previous.created_at, current.created_at)
}

/// Build one row metadata entry per message. `unread_after` is the confirmed
/// read id from before this channel was opened; it is zero when unknown.
pub fn row_meta(messages: &[Message], channel_kind: &str, unread_after: i64) -> Vec<RowMeta> {
    let mut rows = Vec::with_capacity(messages.len());
    let mut previous: Option<&Message> = None;
    for message in messages {
        let grouped = previous.is_some_and(|previous| can_group(previous, message, channel_kind));
        let date = if message.created_at > 0
            && previous.is_none_or(|previous| !same_day(previous.created_at, message.created_at))
        {
            Some(format_date(message.created_at))
        } else {
            None
        };
        let unread = unread_after > 0
            && message.id > unread_after
            && previous.is_none_or(|previous| previous.id <= unread_after);
        rows.push(RowMeta {
            grouped,
            date,
            unread,
        });
        previous = Some(message);
    }
    rows
}

pub fn format_time(value: i64) -> String {
    if value <= 0 {
        return String::new();
    }
    let (_, _, _, hour, minute) = local_parts(value);
    format!("{hour:02}:{minute:02}")
}

pub fn format_date(value: i64) -> String {
    let (year, month, day, _, _) = local_parts(value);
    const MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    let month = MONTHS
        .get(month.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("");
    format!("{day} {month} {year} г.")
}

pub fn role_label(role: &str, short: bool) -> &'static str {
    match role {
        "owner" if short => "ВЛАДЕЛЕЦ",
        "owner" => "Владелец сервера",
        "admin" if short => "АДМИН",
        "admin" => "Администратор",
        _ if short => "УЧАСТНИК",
        _ => "Участник",
    }
}

pub fn role_color(role: &str, light: bool) -> u32 {
    match role {
        "owner" => {
            if light {
                0x805011
            } else {
                0xffc45d
            }
        }
        "admin" => {
            if light {
                0x5b3ebb
            } else {
                0xa993ff
            }
        }
        _ => {
            if light {
                0x0b6878
            } else {
                0x63d8e6
            }
        }
    }
}

/// Go `history_media.go::attachmentSize`.
pub fn attachment_size(width: i32, height: i32, available: f32) -> (f32, f32) {
    let (mut width, mut height) = (width as f32, height as f32);
    if width < 1.0 || height < 1.0 {
        width = 320.0;
        height = 180.0;
    }
    let scale = 1.0_f32.min((340.0_f32.min(available) / width).min(220.0 / height));
    ((width * scale).max(1.0), (height * scale).max(1.0))
}

pub fn image_attachment(file: &voxhold_api::Attachment) -> bool {
    !file.deleted && (file.mime_type == "image/jpeg" || file.mime_type == "image/png")
}

pub fn attachment_label(file: &voxhold_api::Attachment) -> String {
    if file.deleted {
        "Вложение удалено".into()
    } else {
        file.filename.clone()
    }
}

pub fn reaction_count(reaction: &voxhold_api::Reaction) -> String {
    if reaction.count > 999 {
        "999+".into()
    } else {
        reaction.count.to_string()
    }
}

/// Go `history_media.go::quoteLabels`.
pub fn quote_labels(message: &Message) -> (String, String) {
    if let Some(reply) = &message.reply {
        if reply.deleted {
            return (
                "Сообщение удалено".into(),
                "Исходное сообщение больше недоступно".into(),
            );
        }
        return (reply.author.username.clone(), collapse(&reply.content));
    }
    if let Some(forward) = &message.forward {
        return (
            format!("Переслано от {}", forward.author.username),
            String::new(),
        );
    }
    (String::new(), String::new())
}

fn collapse(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Windows local time for an arbitrary Unix timestamp, or UTC on other hosts.
/// `SystemTimeToTzSpecificLocalTime` applies the DST rule of that date.
fn local_parts(value: i64) -> (i32, u32, u32, u32, u32) {
    let value = value.max(0);
    let days = value.div_euclid(86_400);
    let seconds = value.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = (seconds / 3600) as u32;
    let minute = ((seconds % 3600) / 60) as u32;
    #[cfg(windows)]
    {
        if let Some((year, month, day, hour, minute)) =
            windows_local(year, month, day, hour, minute)
        {
            return (year, month, day, hour, minute);
        }
    }
    (year, month, day, hour, minute)
}

#[cfg(windows)]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SystemTimeToTzSpecificLocalTime(
        zone: *const core::ffi::c_void,
        universal: *const SystemTime,
        local: *mut SystemTime,
    ) -> i32;
}

#[cfg(windows)]
fn windows_local(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
) -> Option<(i32, u32, u32, u32, u32)> {
    let universal = SystemTime {
        year: year.try_into().ok()?,
        month: month.try_into().ok()?,
        day: day.try_into().ok()?,
        hour: hour.try_into().ok()?,
        minute: minute.try_into().ok()?,
        ..Default::default()
    };
    let mut local = SystemTime::default();
    let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &universal, &mut local) };
    (ok != 0).then(|| {
        (
            i32::from(local.year),
            u32::from(local.month),
            u32::from(local.day),
            u32::from(local.hour),
            u32::from(local.minute),
        )
    })
}

/// Howard Hinnant's civil date algorithm, used for both UTC and the Win32
/// conversion input.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    (
        (year + i64::from(month <= 2)) as i32,
        month as u32,
        day as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxhold_api::{Attachment, Author, MessageReference};

    fn message(id: i64, user: i64, created_at: i64) -> Message {
        Message {
            id,
            channel_id: 7,
            content: format!("row {id}"),
            author: Author {
                user_id: user,
                username: format!("user {user}"),
            },
            created_at,
            ..Default::default()
        }
    }

    #[test]
    fn groups_only_close_same_author_rows_without_quotes() {
        let first = message(1, 1, 1_000);
        let next = message(2, 1, 1_299);
        assert!(can_group(&first, &next, "text"));
        assert!(!can_group(&first, &message(2, 2, 1_100), "text"));
        assert!(!can_group(&first, &message(2, 1, 1_300), "text"));
        assert!(!can_group(&first, &next, "news"));
        let mut reply = next.clone();
        reply.reply = Some(MessageReference::default());
        assert!(!can_group(&first, &reply, "text"));
        let mut forward = next;
        forward.forward = Some(MessageReference::default());
        assert!(!can_group(&first, &forward, "text"));
    }

    #[test]
    fn date_and_unread_boundaries_land_on_the_first_matching_row() {
        let rows = vec![
            message(10, 1, 43_200),
            message(11, 1, 43_300),
            message(12, 2, 129_600),
            message(13, 2, 129_700),
        ];
        let meta = row_meta(&rows, "text", 10);
        assert!(meta[0].date.is_some());
        assert!(!meta[0].unread);
        assert!(meta[1].unread, "the divider starts at the first unread id");
        assert!(!meta[2].unread, "the divider is drawn exactly once");
        assert!(meta[2].date.is_some(), "a day change adds a date divider");
    }

    #[test]
    fn attachment_geometry_matches_go() {
        assert_eq!(attachment_size(320, 180, 500.0), (320.0, 180.0));
        assert_eq!(attachment_size(1000, 1000, 500.0), (220.0, 220.0));
        assert_eq!(attachment_size(0, 0, 500.0), (320.0, 180.0));
        let image = Attachment {
            id: "a".into(),
            filename: "photo.png".into(),
            mime_type: "image/png".into(),
            ..Default::default()
        };
        assert!(image_attachment(&image));
        assert!(!image_attachment(&Attachment {
            deleted: true,
            ..image.clone()
        }));
        assert_eq!(attachment_label(&image), "photo.png");
    }

    #[test]
    fn quote_labels_follow_go_states() {
        let mut row = message(1, 1, 1_000);
        row.reply = Some(MessageReference {
            message_id: 3,
            author: Author {
                user_id: 2,
                username: "Лера".into(),
            },
            content: "  первая\nстрока  ".into(),
            deleted: false,
        });
        assert_eq!(quote_labels(&row), ("Лера".into(), "первая строка".into()));
        row.reply.as_mut().unwrap().deleted = true;
        assert_eq!(
            quote_labels(&row),
            (
                "Сообщение удалено".into(),
                "Исходное сообщение больше недоступно".into()
            )
        );
        row.reply = None;
        row.forward = Some(MessageReference {
            author: Author {
                user_id: 4,
                username: "Миша".into(),
            },
            ..Default::default()
        });
        assert_eq!(quote_labels(&row).0, "Переслано от Миша");
    }

    #[test]
    fn role_tokens_match_go() {
        assert_eq!(role_label("owner", true), "ВЛАДЕЛЕЦ");
        assert_eq!(role_label("admin", false), "Администратор");
        assert_eq!(role_label("member", true), "УЧАСТНИК");
        assert_eq!(role_color("owner", false), 0xffc45d);
        assert_eq!(role_color("admin", true), 0x5b3ebb);
    }

    #[test]
    fn civil_dates_are_stable() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_771), (2026, 11, 14));
        assert_eq!(format_time(0), "");
    }
}
