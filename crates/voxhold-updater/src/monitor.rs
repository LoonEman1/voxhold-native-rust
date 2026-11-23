//! Work area rectangle passed to the helper with `--monitor`.
//!
//! The client formats the `rcWork` of the monitor that showed its window as
//! `left,top,right,bottom` in physical pixels. The helper uses the rectangle to
//! centre its own window and ignores it when it no longer intersects a live
//! monitor (for example after a monitor was unplugged).

/// Rectangle in physical pixels, as reported by `GetMonitorInfoW`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl MonitorRect {
    /// Parse `left,top,right,bottom`. Rejects a reversed or empty rectangle.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split(',');
        let left = parts.next()?.trim().parse().ok()?;
        let top = parts.next()?.trim().parse().ok()?;
        let right = parts.next()?.trim().parse().ok()?;
        let bottom = parts.next()?.trim().parse().ok()?;
        if parts.next().is_some() || right <= left || bottom <= top {
            return None;
        }
        Some(Self {
            left,
            top,
            right,
            bottom,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_rect_parsing_is_strict() {
        assert_eq!(
            MonitorRect::parse("0,0,1920,1040"),
            Some(MonitorRect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040,
            })
        );
        assert_eq!(
            MonitorRect::parse("-1920, 0, 0, 1080"),
            Some(MonitorRect {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            })
        );
        for invalid in [
            "",
            "0,0,1920",
            "0,0,1920,1040,1",
            "0,0,0,1040",
            "0,0,1920,0",
            "left,top,right,bottom",
        ] {
            assert_eq!(MonitorRect::parse(invalid), None, "{invalid}");
        }
    }
}
