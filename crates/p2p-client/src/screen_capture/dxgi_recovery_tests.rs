use super::*;
use std::cell::Cell;
use windows::Win32::Foundation::{E_ACCESSDENIED, E_INVALIDARG};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_SESSION_DISCONNECTED,
    DXGI_ERROR_UNSUPPORTED,
};

fn windows_error(code: windows::core::HRESULT) -> DxgiCaptureError {
    DxgiCaptureError::WindowsError(windows::core::Error::from_hresult(code))
}

#[test]
fn secure_desktop_recovery_survives_repeated_denials_until_access_returns() {
    let stop = AtomicBool::new(false);
    let mut attempts = 0;
    let waits = Cell::new(0);
    let result = retry_dxgi_recreation(
        &stop,
        || {
            attempts += 1;
            // More than one prompt-length delay must not terminate the worker.
            if attempts <= 20 {
                Err(windows_error(E_ACCESSDENIED))
            } else {
                Ok(42)
            }
        },
        || waits.set(waits.get() + 1),
    );
    assert_eq!(result.unwrap(), Some(42));
    assert_eq!(attempts, 21);
    assert_eq!(waits.get(), 20);
}

#[test]
fn desktop_switch_recovery_retries_transient_mode_and_session_errors() {
    let stop = AtomicBool::new(false);
    let mut failures = [
        DxgiCaptureError::AccessLost,
        windows_error(DXGI_ERROR_ACCESS_LOST),
        windows_error(DXGI_ERROR_ACCESS_DENIED),
        windows_error(DXGI_ERROR_SESSION_DISCONNECTED),
        windows_error(DXGI_ERROR_UNSUPPORTED),
        DxgiCaptureError::DirectXError(windows_capture::d3d11::Error::WindowsError(
            windows::core::Error::from_hresult(E_ACCESSDENIED),
        )),
    ]
    .into_iter();
    let result = retry_dxgi_recreation(&stop, || failures.next().map_or(Ok("resumed"), Err), || {});
    assert_eq!(result.unwrap(), Some("resumed"));
}

#[test]
fn capture_recovery_preserves_non_transient_errors() {
    let stop = AtomicBool::new(false);
    let attempts = Cell::new(0);
    let result = retry_dxgi_recreation::<()>(
        &stop,
        || {
            attempts.set(attempts.get() + 1);
            Err(windows_error(E_INVALIDARG))
        },
        || panic!("permanent errors must not be retried"),
    );
    assert!(
        matches!(result, Err(DxgiCaptureError::WindowsError(error)) if error.code() == E_INVALIDARG)
    );
    assert_eq!(attempts.get(), 1);
}

#[test]
fn capture_recovery_can_be_stopped_before_or_during_a_protected_desktop() {
    let stop = AtomicBool::new(true);
    assert!(
        retry_dxgi_recreation::<()>(
            &stop,
            || panic!("cancelled capture must not be restarted"),
            || panic!("cancelled capture must not wait"),
        )
        .unwrap()
        .is_none()
    );

    stop.store(false, Ordering::Relaxed);
    let attempts = Cell::new(0);
    let result = retry_dxgi_recreation::<()>(
        &stop,
        || {
            attempts.set(attempts.get() + 1);
            Err(windows_error(E_ACCESSDENIED))
        },
        || stop.store(true, Ordering::Relaxed),
    );
    assert!(result.unwrap().is_none());
    assert_eq!(attempts.get(), 1);
}

#[test]
fn protected_desktop_does_not_finish_capture_and_new_frames_resume_in_the_same_store() {
    let stop = Arc::new(AtomicBool::new(false));
    let latest_frame = LatestFrame::default();
    let make_frame = |sequence| PreviewFrame {
        sequence,
        width: 2,
        height: 2,
        rgba: vec![sequence as u8; 16],
        gpu_nv12: None,
        cpu_nv12: None,
    };
    latest_frame.publish(make_frame(1));
    let worker_stop = Arc::clone(&stop);
    let worker_frames = latest_frame.clone();
    let (state_tx, state_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut attempts = 0;
        let resumed = retry_dxgi_recreation(
            &worker_stop,
            || {
                attempts += 1;
                if attempts == 1 {
                    Err(windows_error(E_ACCESSDENIED))
                } else {
                    Ok(())
                }
            },
            || {
                state_tx.send("paused").unwrap();
                resume_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            },
        )
        .map_err(|error| error.to_string())?;
        if resumed.is_some() {
            worker_frames.publish(make_frame(2));
            state_tx.send("resumed").unwrap();
        }
        while !worker_stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    });
    let mut capture = ScreenCapture {
        backend_name: "DXGI Desktop Duplication",
        control: None,
        dxgi_worker: Some(DxgiCaptureWorker {
            stop,
            thread: Some(worker),
        }),
        latest_frame,
        performance: Arc::new(CapturePerformanceCounters::default()),
        fallback_reason: Arc::new(Mutex::new(None)),
        source_closed: Arc::new(AtomicBool::new(false)),
        picker_owner: None,
        quality: ScreenShareQuality::default(),
    };
    assert_eq!(
        state_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        "paused"
    );
    assert!(capture.poll_finished().is_none());
    assert!(!capture.source_closed());
    assert_eq!(capture.latest_frame().unwrap().sequence, 1);
    resume_tx.send(()).unwrap();
    assert_eq!(
        state_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        "resumed"
    );
    assert!(capture.poll_finished().is_none());
    assert_eq!(capture.latest_frame().unwrap().sequence, 2);
    capture.stop().unwrap();
}
