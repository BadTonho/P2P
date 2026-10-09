use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::{Client, Response};
use reqwest::header::CONTENT_TYPE;
use reqwest::redirect::Policy;
use serde::Deserialize;
use sha2::{Digest, Sha256};

mod apply;
mod download;
mod release;
#[cfg(test)]
mod test_support;

pub use apply::{cleanup_stale_helpers, helper_arguments, stage_and_launch};

const GITHUB_OWNER: &str = "BadTonho";
const GITHUB_REPOSITORY: &str = "P2P";
const GITHUB_RELEASE_API_URL: &str = "https://api.github.com/repos/BadTonho/P2P/releases/latest";
const GITHUB_RELEASES_PAGE_URL: &str = "https://github.com/BadTonho/P2P/releases/latest";
const RELEASE_ASSET_NAME: &str = "p2p-client.exe";
const RELEASE_MAX_BYTES: u64 = 2 * 1024 * 1024;
const UPDATE_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const UPDATE_DIRECTORY: &str = "P2P-Voz-e-tela\\updates";
const BUILD_VERSION_MARKER_PREFIX: &[u8] = b"P2P_VOZ_E_TELA_BUILD_VERSION=";
const MAX_BUILD_VERSION_BYTES: usize = 64;

#[used]
static EMBEDDED_BUILD_VERSION_MARKER: &[u8] = concat!(
    "P2P_VOZ_E_TELA_BUILD_VERSION=",
    env!("CARGO_PKG_VERSION"),
    "\0"
)
.as_bytes();

pub fn build_version_marker() -> &'static str {
    std::str::from_utf8(EMBEDDED_BUILD_VERSION_MARKER)
        .expect("version marker is valid UTF-8")
        .trim_end_matches('\0')
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateManifest {
    pub version: String,
    pub download_url: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug)]
pub enum UpdateEvent {
    CheckFinished(Result<Option<UpdateManifest>, String>),
    DownloadProgress {
        version: String,
        received: u64,
    },
    DownloadFinished {
        manifest: UpdateManifest,
        path: PathBuf,
    },
    DownloadCancelled {
        version: String,
    },
    DownloadFailed {
        version: String,
        error: String,
    },
    ApplyStarted,
    ApplyFailed(String),
}

#[derive(Default)]
pub struct UpdateManager {
    sender: Option<Sender<UpdateEvent>>,
    receiver: Option<Receiver<UpdateEvent>>,
    cancel_download: Option<Arc<AtomicBool>>,
}

impl UpdateManager {
    fn channel(&mut self) -> Sender<UpdateEvent> {
        if self.sender.is_none() {
            let (sender, receiver) = mpsc::channel();
            self.sender = Some(sender);
            self.receiver = Some(receiver);
        }
        self.sender.as_ref().expect("update event sender").clone()
    }

    pub fn releases_page_url() -> &'static str {
        GITHUB_RELEASES_PAGE_URL
    }

    pub fn check(&mut self) {
        let sender = self.channel();
        tracing::info!(
            current_version = env!("CARGO_PKG_VERSION"),
            "Verificando atualizações"
        );
        thread::spawn(move || {
            let result = release::check_for_update();
            match &result {
                Ok(Some(manifest)) => tracing::info!(
                    version = %manifest.version,
                    "Nova versão disponível"
                ),
                Ok(None) => tracing::info!("Aplicativo já está na versão mais recente"),
                Err(error) => tracing::warn!(error = %error, "Falha ao verificar atualizações"),
            }
            let _ = sender.send(UpdateEvent::CheckFinished(result));
        });
    }

    pub fn download(&mut self, manifest: UpdateManifest) {
        self.cancel_download();
        let cancellation = Arc::new(AtomicBool::new(false));
        self.cancel_download = Some(cancellation.clone());
        let sender = self.channel();
        thread::spawn(move || download::download_update(manifest, cancellation, sender));
    }

    pub fn cancel_download(&mut self) {
        if let Some(cancel) = self.cancel_download.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn apply(&mut self, downloaded: PathBuf, expected_version: String) {
        let sender = self.channel();
        thread::spawn(move || {
            let result = apply::stage_and_launch(downloaded, expected_version);
            match result {
                Ok(()) => {
                    let _ = sender.send(UpdateEvent::ApplyStarted);
                }
                Err(error) => {
                    let _ = sender.send(UpdateEvent::ApplyFailed(error));
                }
            }
        });
    }

    pub fn try_recv(&mut self) -> Option<UpdateEvent> {
        self.receiver.as_ref()?.try_recv().ok()
    }
}
