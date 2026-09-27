use std::fs::{self, File};
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use signaling_protocol::RoomMode;

const SETTINGS_DIRECTORY: &str = "P2P-Voz-e-tela";
const SETTINGS_FILE: &str = "settings.json";
const SETTINGS_SCHEMA_VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 128 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub(crate) schema_version: u32,
    pub server_url: String,
    pub stun_server_url: String,
    pub monitor_gain_db: f32,
    pub create_room_mode: RoomMode,
    pub use_turn_on_create: bool,
    pub may_host: bool,
    pub control_ipv4: Option<Ipv4Addr>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            server_url: String::new(),
            stun_server_url: "stun:stun.l.google.com:19302".to_owned(),
            monitor_gain_db: 6.0,
            create_room_mode: RoomMode::Local,
            use_turn_on_create: true,
            may_host: false,
            control_ipv4: None,
        }
    }
}

pub fn load() -> (AppSettings, Option<String>) {
    let path = match settings_path() {
        Ok(path) => path,
        Err(error) => return (AppSettings::default(), Some(error)),
    };
    load_from(&path)
}

pub fn save(settings: &AppSettings) -> Result<(), String> {
    let path = settings_path()?;
    save_to(&path, settings).map_err(|error| {
        format!(
            "Não foi possível salvar as preferências em '{}': {error}",
            path.display()
        )
    })
}

fn settings_path() -> Result<PathBuf, String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "A variável LOCALAPPDATA não está definida.".to_owned())?;
    Ok(local_app_data.join(SETTINGS_DIRECTORY).join(SETTINGS_FILE))
}

fn load_from(path: &Path) -> (AppSettings, Option<String>) {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (AppSettings::default(), None);
        }
        Err(error) => {
            return (
                AppSettings::default(),
                Some(format!(
                    "Não foi possível ler as preferências de '{}': {error}. Os valores padrão foram carregados.",
                    path.display()
                )),
            );
        }
    };

    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return (
            AppSettings::default(),
            Some("O arquivo de preferências é maior que o limite permitido. Os valores padrão foram carregados.".to_owned()),
        );
    }

    let mut settings: AppSettings = match serde_json::from_slice(&bytes) {
        Ok(settings) => settings,
        Err(error) => {
            return (
                AppSettings::default(),
                Some(format!(
                    "O arquivo de preferências está inválido ({error}). Os valores padrão foram carregados."
                )),
            );
        }
    };

    if settings.schema_version != SETTINGS_SCHEMA_VERSION {
        return (
            AppSettings::default(),
            Some("O arquivo de preferências usa uma versão incompatível. Os valores padrão foram carregados.".to_owned()),
        );
    }

    let default_gain = AppSettings::default().monitor_gain_db;
    if !settings.monitor_gain_db.is_finite() {
        settings.monitor_gain_db = default_gain;
    } else {
        settings.monitor_gain_db = settings.monitor_gain_db.clamp(0.0, 18.0);
    }
    (settings, None)
}

fn save_to(path: &Path, settings: &AppSettings) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "caminho sem pasta"))?;
    fs::create_dir_all(parent)?;

    let mut stored = settings.clone();
    stored.schema_version = SETTINGS_SCHEMA_VERSION;
    if !stored.monitor_gain_db.is_finite() {
        stored.monitor_gain_db = AppSettings::default().monitor_gain_db;
    } else {
        stored.monitor_gain_db = stored.monitor_gain_db.clamp(0.0, 18.0);
    }

    let temporary_path = path.with_extension("json.tmp");
    let mut file = File::create(&temporary_path)?;
    let serialized = serde_json::to_vec_pretty(&stored)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    file.write_all(&serialized)?;
    file.sync_all()?;
    drop(file);

    if let Err(error) = atomic_replace(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing_file: *const u16, new_file: *const u16, flags: u32) -> i32;
    }

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after UNIX epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("p2p-settings-test-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn settings_round_trip_atomically() {
        let directory = temporary_directory();
        let path = directory.join(SETTINGS_FILE);
        let settings = AppSettings {
            server_url: "192.168.1.20".to_owned(),
            monitor_gain_db: 12.0,
            create_room_mode: RoomMode::InternetTest,
            may_host: true,
            control_ipv4: Some(Ipv4Addr::new(26, 10, 20, 30)),
            ..AppSettings::default()
        };

        save_to(&path, &settings).expect("settings should save");
        let (loaded, warning) = load_from(&path);
        assert_eq!(loaded, settings);
        assert!(warning.is_none());
        fs::remove_dir_all(directory).expect("test directory should be removed");
    }

    #[test]
    fn invalid_file_uses_defaults_without_panicking() {
        let directory = temporary_directory();
        fs::create_dir_all(&directory).expect("test directory should be created");
        let path = directory.join(SETTINGS_FILE);
        fs::write(&path, b"not json").expect("invalid settings should be written");

        let (loaded, warning) = load_from(&path);
        assert_eq!(loaded, AppSettings::default());
        assert!(warning.is_some());
        fs::remove_dir_all(directory).expect("test directory should be removed");
    }

    #[test]
    fn settings_do_not_serialize_room_codes_or_turn_credentials() {
        let serialized =
            serde_json::to_string(&AppSettings::default()).expect("settings should serialize");
        assert!(!serialized.contains("room_code"));
        assert!(!serialized.contains("turn_credentials"));
        assert!(!serialized.contains("password"));
    }
}
