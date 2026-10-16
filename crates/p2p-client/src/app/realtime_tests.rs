use super::*;
use std::mem::ManuallyDrop;

fn app() -> ManuallyDrop<ClientUi> {
    // Avoid devices, network discovery, update checks and saved user preferences.
    let mut app = ManuallyDrop::new(ClientUi::default());
    app.monitors_loaded = true;
    app.addresses_loaded = true;
    app.last_adapter_refresh = Some(Instant::now());
    app.update_status = UpdateStatus::UpToDate;
    app.room_mode = RoomMode::InternetTest;
    app.create_room_mode = RoomMode::InternetTest;
    app
}

fn frame(sequence: u64) -> Arc<PreviewFrame> {
    Arc::new(PreviewFrame {
        sequence,
        width: 2,
        height: 2,
        rgba: [sequence as u8, 80, 160, 255].repeat(4),
        #[cfg(windows)]
        gpu_nv12: None,
        #[cfg(windows)]
        cpu_nv12: None,
    })
}

#[test]
fn hidden_video_keeps_updating_in_both_directions_without_mouse_input() {
    use eframe::App as _;

    let contexts = [egui::Context::default(), egui::Context::default()];
    let mut apps = [app(), app()];
    let mut stores = Vec::new();
    for app in &mut apps {
        let (session, store) = ScreenShareSession::synthetic_video_receiver();
        app.screen_share_session = Some(session);
        app.screen_share_role = ScreenShareRole::Receiving {
            request_id: "synthetic-video".to_owned(),
        };
        stores.push(store);
    }
    let mut input = egui::RawInput::default();
    input
        .viewports
        .get_mut(&egui::ViewportId::ROOT)
        .unwrap()
        .minimized = Some(true);
    for sequence in 1..=5 {
        for ((app, context), store) in apps.iter_mut().zip(&contexts).zip(&stores) {
            *store.lock().unwrap() = Some(frame(sequence));
            let _ = context.run_logic(&input, |ctx| {
                app.logic(ctx, &mut eframe::Frame::_new_kittest());
            });
            assert_eq!(app.remote_screen_sequence, sequence);
            assert!(app.remote_screen_texture.is_some());
        }
    }
    for app in &mut apps {
        app.screen_share_session = None;
    }
}

#[test]
fn hidden_group_video_consumes_new_frames_without_drawing_the_window() {
    use eframe::App as _;

    let context = egui::Context::default();
    let mut app = app();
    let (session, store) = ScreenShareSession::synthetic_video_receiver();
    app.group_share
        .inbound_sessions
        .insert("peer".to_owned(), session);
    for sequence in 1..=5 {
        *store.lock().unwrap() = Some(frame(sequence));
        let _ = context.run_logic(&egui::RawInput::default(), |ctx| {
            app.logic(ctx, &mut eframe::Frame::_new_kittest());
        });
        assert_eq!(
            app.group_share.remote_sequences.get("peer"),
            Some(&sequence)
        );
    }
    app.group_share.inbound_sessions.clear();
}

#[test]
fn active_video_schedules_another_frame_at_video_cadence_without_input() {
    use eframe::App as _;

    let context = egui::Context::default();
    let mut app = app();
    let (session, _) = ScreenShareSession::synthetic_video_receiver();
    app.screen_share_session = Some(session);
    let mut delays = Vec::new();
    // Let the initial layout settle, so automatic startup repaints cannot hide
    // the missing video refresh timer.
    for _ in 0..6 {
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            app.logic(ui.ctx(), &mut eframe::Frame::_new_kittest());
            app.ui(ui, &mut eframe::Frame::_new_kittest());
        });
        delays.push(output.viewport_output[&egui::ViewportId::ROOT].repaint_delay);
        output.textures_delta.clear();
    }
    assert!(
        delays[3..]
            .iter()
            .all(|delay| *delay <= Duration::from_secs_f64(1.0 / 60.0)),
        "video repaint delays: {delays:?}"
    );
    app.screen_share_session = None;
}
