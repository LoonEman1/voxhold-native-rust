use super::*;

#[test]
fn process_loopback_params_carry_the_target_and_the_include_mode() {
    let params = process_loopback_params(4_321, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE);
    assert_eq!(
        params.ActivationType,
        AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
    );
    let inner = unsafe { params.Anonymous.ProcessLoopbackParams };
    assert_eq!(inner.TargetProcessId, 4_321);
    assert_eq!(
        inner.ProcessLoopbackMode,
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
    );

    let excluded =
        process_loopback_params(4_321, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE);
    let inner = unsafe { excluded.Anonymous.ProcessLoopbackParams };
    assert_eq!(
        inner.ProcessLoopbackMode,
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
    );
    assert_ne!(
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
    );
}

#[test]
fn the_activation_blob_describes_the_exact_parameter_size() {
    let mut params = process_loopback_params(7, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE);
    let variant = activation_blob(&mut params);
    let inner = unsafe { &*variant.Anonymous.Anonymous };
    assert_eq!(inner.vt, VT_BLOB);
    let blob = unsafe { inner.Anonymous.blob };
    assert_eq!(
        blob.cbSize as usize,
        size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()
    );
    assert_eq!(blob.pBlobData, ptr::from_mut(&mut params).cast());
    let read_back = unsafe { *(blob.pBlobData.cast::<AUDIOCLIENT_ACTIVATION_PARAMS>()) };
    let read_inner = unsafe { read_back.Anonymous.ProcessLoopbackParams };
    assert_eq!(read_inner.TargetProcessId, 7);
    // The `ManuallyDrop` wrapper is intentionally not unwrapped: the blob
    // borrows this stack value and must not be freed by `PropVariantClear`.
}

#[test]
fn the_keepalive_blob_points_into_its_owned_params_after_moving() {
    let keepalive = ActivationKeepAlive::new(7, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE);
    let variant_before = keepalive.variant_ptr();
    // Moving the keepalive must not move the boxed variant or params: the
    // COM activation keeps both pointers past this function's stack frame.
    let mut moved = Box::new(keepalive);
    assert_eq!(
        moved.variant_ptr(),
        variant_before,
        "the boxed variant keeps its address when the keepalive moves"
    );
    let inner = unsafe { &*moved.variant().Anonymous.Anonymous };
    assert_eq!(inner.vt, VT_BLOB);
    let blob = unsafe { inner.Anonymous.blob };
    assert_eq!(
        blob.cbSize as usize,
        size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>()
    );
    assert_eq!(
        blob.pBlobData,
        ptr::from_mut(&mut *moved._params).cast(),
        "the blob points into the owned params allocation"
    );
    let read_back = unsafe { *(blob.pBlobData.cast::<AUDIOCLIENT_ACTIVATION_PARAMS>()) };
    let read_inner = unsafe { read_back.Anonymous.ProcessLoopbackParams };
    assert_eq!(read_inner.TargetProcessId, 7);
}

#[test]
fn a_timed_out_activation_does_not_release_the_pending_guard() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DropProbe(Arc<AtomicBool>);
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    impl LeakOnTimeout for DropProbe {
        fn leak(self) {
            std::mem::forget(self);
        }
    }

    let dropped = Arc::new(AtomicBool::new(false));
    // Keep the sender alive: this is a real timeout, not a disconnect.
    let (_sender, receiver) = mpsc::channel::<Result<(), String>>();
    let error = await_activation(
        &receiver,
        Duration::from_millis(1),
        DropProbe(dropped.clone()),
    )
    .expect_err("an activation that never answers is a timeout");
    assert!(error.contains("не завершилась"), "{error}");
    assert!(
        !dropped.load(Ordering::SeqCst),
        "the pending COM guard must stay alive after the timeout"
    );
}

#[test]
fn an_activation_failure_is_returned_instead_of_opening_the_device() {
    let mut asked = None;
    let result = open_process_with(
        4_242,
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
        |pid, mode| {
            asked = Some((pid, mode));
            Err("fake activation failure".into())
        },
    );
    let error = match result {
        Ok(_) => panic!("a failed activation must not open a capture"),
        Err(error) => error,
    };
    assert_eq!(
        asked,
        Some((4_242, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE)),
        "the requested process and mode reach the activation call"
    );
    match error {
        Failure::Fatal(message) => {
            assert!(
                message.contains("fake activation failure"),
                "the activation error must propagate unchanged: {message}"
            );
            assert!(
                !message.contains("Устройство вывода по умолчанию"),
                "the failure must not describe a whole-device capture: {message}"
            );
        }
        Failure::Reopen(message) => {
            panic!("an activation failure is terminal, not a reopen: {message}")
        }
    }
}

#[test]
fn a_window_id_resolves_to_the_owning_process() {
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
        },
        core::w,
    };
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("voxhold-scope-test"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    };
    let window = match window {
        Ok(window) => window,
        // A session without a window station cannot create even a
        // message-only window; the pure resolution is still covered above.
        Err(error) => {
            println!("wasapi: no test window available: {error}");
            return;
        }
    };
    let pid = window_process_id(window.0 as u64).expect("the test window has a process");
    assert_eq!(pid, std::process::id());
    assert!(process_name(pid).is_some_and(|name| !name.is_empty()));
    let _ = unsafe { DestroyWindow(window) };
}

#[test]
fn an_absent_window_is_an_explicit_error() {
    assert!(window_process_id(0).is_err());
}

#[test]
fn an_unknown_endpoint_id_does_not_report_a_default_device_change() {
    const DEFAULT: &str = "{0.0.0.00000000}.{real-endpoint}";
    assert!(
        !default_device_changed(DEFAULT, ""),
        "an unreadable opened id is unknown, not a moved device"
    );
    assert!(
        !default_device_changed(DEFAULT, DEFAULT),
        "the same endpoint is not a change"
    );
    assert!(
        default_device_changed(DEFAULT, "{0.0.0.00000000}.{other}"),
        "a genuinely different endpoint is a change"
    );
}
