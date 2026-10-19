#![windows_subsystem = "windows"]

mod app;
mod audio_capture;
mod control_mesh;
mod discovery;
mod gpu_telemetry;
mod logging;
mod mesh_identity;
mod mf_video;
mod nv12;
mod profile;
mod screen_capture;
mod screen_quality;
mod screen_sharing;
mod settings;
mod signaling_client;
mod turn_relay;
mod update;
mod video_performance;

#[cfg(test)]
#[path = "source_hygiene.rs"]
mod source_hygiene_tests;

fn main() -> eframe::Result {
    app::run()
}
