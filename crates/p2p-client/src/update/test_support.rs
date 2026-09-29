use super::{BUILD_VERSION_MARKER_PREFIX, UpdateManifest};
use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn test_directory() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("p2p-updater-test-{nonce}"));
    fs::create_dir_all(&path).unwrap();
    path
}

pub(super) fn manifest() -> UpdateManifest {
    UpdateManifest {
        version: "1.0.1".to_owned(),
        download_url: "https://github.com/BadTonho/P2P/releases/download/v1.0.1/p2p-client.exe"
            .to_owned(),
        size_bytes: 123,
        sha256: "a".repeat(64),
    }
}

pub(super) fn executable_stub(version: &str) -> Vec<u8> {
    [
        b"MZfake executable".as_slice(),
        BUILD_VERSION_MARKER_PREFIX,
        version.as_bytes(),
        b"\0",
    ]
    .concat()
}
