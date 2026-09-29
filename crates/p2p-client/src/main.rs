#![windows_subsystem = "windows"]

mod app;
mod audio_capture;
mod control_mesh;
mod logging;
mod mf_video;
mod profile;
mod screen_capture;
mod screen_sharing;
mod settings;
mod signaling_client;
mod turn_relay;
mod update;

fn main() -> eframe::Result {
    app::run()
}
