//! Open an http(s) link in the system browser, like Go `View.OpenURL`
//! (`ShellExecuteW` on Windows). Only absolute http/https URLs with a host are
//! accepted, so a message cannot launch a local file or a custom protocol.

use url::Url;

/// Whether the href may be opened by the shell.
pub fn openable(href: &str) -> bool {
    let Ok(url) = Url::parse(href) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some_and(|host| !host.is_empty())
}

/// Open the link in the default browser. The error text matches Go and never
/// contains the URL.
pub fn open(href: &str) -> Result<(), String> {
    if !openable(href) {
        return Err("Не удалось открыть ссылку.".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "shell32")]
        unsafe extern "system" {
            fn ShellExecuteW(
                window: *mut core::ffi::c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> *mut core::ffi::c_void;
        }

        let wide = |value: &std::ffi::OsStr| -> Vec<u16> {
            value.encode_wide().chain(std::iter::once(0)).collect()
        };
        let operation = wide(std::ffi::OsStr::new("open"));
        let target = wide(std::ffi::OsStr::new(href));
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        } as isize;
        // ShellExecuteW returns a value greater than 32 on success.
        if result > 32 {
            Ok(())
        } else {
            Err("Не удалось открыть ссылку.".into())
        }
    }
    #[cfg(not(windows))]
    {
        let _ = href;
        Err("Не удалось открыть ссылку.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_absolute_web_links_are_openable() {
        assert!(openable("https://example.com/a"));
        assert!(openable("http://127.0.0.1:8080/health"));
        assert!(!openable("file:///C:/Windows/win.ini"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("mailto:user@example.com"));
        assert!(!openable("/relative/path"));
        assert!(!openable(""));
        assert!(!openable("https://"));
    }
}
