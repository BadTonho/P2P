use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use time::{Date, Duration, OffsetDateTime};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::prelude::*;

const LOG_FOLDER_NAME: &str = "P2P-Voz-e-tela";
const LOG_FILE_PREFIX: &str = "p2p-";
const RETENTION_DAYS: i64 = 14;

#[derive(Default)]
pub struct LoggingState {
    log_directory: Option<PathBuf>,
    last_write_error: Option<Arc<Mutex<Option<String>>>>,
    pub startup_message: Option<String>,
    pub export_message: Option<String>,
}

pub fn install_panic_hook() {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        if let Some(location) = panic.location() {
            tracing::error!(
                file = location.file(),
                line = location.line(),
                column = location.column(),
                "Aplicativo entrou em panic; conteúdo da mensagem omitido"
            );
        } else {
            tracing::error!("Aplicativo entrou em panic; localização e conteúdo indisponíveis");
        }
        previous_hook(panic);
    }));
}

#[derive(Default)]
pub struct DiagnosticSnapshot {
    pub log_directory: String,
    pub logging_status: String,
    pub room_mode: String,
    pub connected_to_signaling: bool,
    pub participant_count: usize,
    pub signaling_address: String,
    pub stun_server: String,
    pub microphone_active: bool,
    pub microphone_level_dbfs: f32,
    pub microphone_gain_db: f32,
    pub adapters: Vec<(String, String)>,
    pub screen_share_state: String,
    pub screen_metrics: String,
}

#[derive(Clone)]
struct SharedLogWriter {
    sink: Arc<Mutex<RollingSink>>,
    last_error: Arc<Mutex<Option<String>>>,
}

struct RollingSink {
    directory: PathBuf,
    current_date: Option<String>,
    file: Option<File>,
}

struct LogLine {
    writer: SharedLogWriter,
    bytes: Vec<u8>,
}

impl LoggingState {
    pub fn initialize() -> Self {
        let (log_directory, startup_message, rolling_sink) = match open_log_directory() {
            Ok((directory, startup_message, sink)) => {
                (Some(directory), startup_message, Some(sink))
            }
            Err((primary_error, fallback_error)) => (
                None,
                Some(format!(
                    "Não foi possível iniciar os logs persistentes. Pasta principal: {primary_error}. Pasta temporária: {fallback_error}."
                )),
                None,
            ),
        };

        let Some(directory) = log_directory else {
            let mut state = Self {
                startup_message,
                ..Self::default()
            };
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_timer(UtcTime::rfc_3339())
                .with_max_level(tracing::Level::DEBUG)
                .finish();
            if let Err(error) = subscriber.try_init() {
                state.startup_message = Some(format!(
                    "{} Logger indisponível: {error}",
                    state.startup_message.unwrap_or_default()
                ));
            }
            return state;
        };

        let sink = Arc::new(Mutex::new(
            rolling_sink.expect("directory implies an opened sink"),
        ));
        let last_error = Arc::new(Mutex::new(None));
        let writer = SharedLogWriter {
            sink,
            last_error: Arc::clone(&last_error),
        };
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_timer(UtcTime::rfc_3339())
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(writer)
            .finish();

        let mut state = Self {
            log_directory: Some(directory),
            last_write_error: Some(last_error),
            startup_message,
            export_message: None,
        };
        if let Err(error) = subscriber.try_init() {
            state.startup_message = Some(format!("Não foi possível ativar o logger: {error}"));
        }

        tracing::info!(
            app_version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS,
            architecture = std::env::consts::ARCH,
            log_directory = state.log_directory_label(),
            "Aplicativo iniciado; logger persistente pronto"
        );
        state
    }

    pub fn take_write_error(&self) -> Option<String> {
        let last_error = self.last_write_error.as_ref()?;
        last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    pub fn export_to(&self, destination: &Path, snapshot: &DiagnosticSnapshot) -> io::Result<()> {
        let mut output = String::new();
        let timestamp = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "horário indisponível".to_owned());
        output.push_str("Diagnóstico do P2P - Voz e tela\n");
        output.push_str(&format!("Exportado em UTC: {timestamp}\n"));
        output.push_str(&format!(
            "Versão do aplicativo: {}\n",
            env!("CARGO_PKG_VERSION")
        ));
        output.push_str(&format!(
            "Sistema: {} {}\n",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        output.push_str(&format!("Pasta de logs: {}\n", snapshot.log_directory));
        output.push_str(&format!("Estado do logger: {}\n", snapshot.logging_status));
        output.push_str(&format!("Modo da sala: {}\n", snapshot.room_mode));
        output.push_str(&format!(
            "Conectado à sinalização: {}\nParticipantes: {}\n",
            snapshot.connected_to_signaling, snapshot.participant_count
        ));
        output.push_str(&format!(
            "Endereço de sinalização: {}\n",
            safe_signaling_endpoint(&snapshot.signaling_address)
        ));
        output.push_str(&format!(
            "Servidor STUN: {}\n",
            safe_stun_endpoint(&snapshot.stun_server)
        ));
        output.push_str(&format!(
            "Teste do microfone ativo: {}\nNível do microfone: {:.1} dBFS\nGanho do retorno local: {:.1} dB\n",
            snapshot.microphone_active,
            snapshot.microphone_level_dbfs,
            snapshot.microphone_gain_db
        ));
        output.push_str(&format!(
            "Estado do compartilhamento: {}\n",
            snapshot.screen_share_state
        ));
        output.push_str(&format!("Métricas de vídeo: {}\n", snapshot.screen_metrics));
        output.push_str("Adaptadores e IPv4 visíveis pelo aplicativo:\n");
        if snapshot.adapters.is_empty() {
            output.push_str("  (nenhum adaptador IPv4 encontrado)\n");
        } else {
            for (adapter, ipv4) in &snapshot.adapters {
                output.push_str(&format!("  {adapter}: {ipv4}\n"));
            }
        }
        output.push_str("\nDados excluídos: código da sala, tokens, payloads de sinalização e conteúdo de áudio/vídeo/tela.\n");
        output.push_str("\n========== LOGS RETIDOS ==========\n");

        if let Some(directory) = &self.log_directory {
            let mut files = log_files(directory)?;
            files.sort();
            for file in files {
                output.push_str(&format!(
                    "\n========== {} ==========\n",
                    file.file_name().unwrap_or_default().to_string_lossy()
                ));
                output.push_str(&fs::read_to_string(file)?);
                if !output.ends_with('\n') {
                    output.push('\n');
                }
            }
        } else {
            output.push_str("Logger persistente indisponível nesta execução.\n");
        }

        if let (Some(parent), Some(log_directory)) = (destination.parent(), self.log_directory()) {
            let parent = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
            let log_directory =
                fs::canonicalize(log_directory).unwrap_or_else(|_| log_directory.to_path_buf());
            if parent == log_directory {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Escolha uma pasta diferente da pasta interna de logs.",
                ));
            }
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, output)
    }

    pub fn log_directory(&self) -> Option<&Path> {
        self.log_directory.as_deref()
    }

    pub fn log_directory_label(&self) -> &'static str {
        if self.log_directory.is_none() {
            "(indisponível)"
        } else if self
            .startup_message
            .as_deref()
            .is_some_and(|message| message.contains("pasta temporária"))
        {
            "%TEMP%\\P2P-Voz-e-tela\\logs-fallback"
        } else {
            "%LOCALAPPDATA%\\P2P-Voz-e-tela\\logs"
        }
    }
}

pub fn safe_stun_endpoint(input: &str) -> String {
    let input = input.trim();
    let Some(rest) = input.strip_prefix("stun:") else {
        return "(inválido)".to_owned();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    format!("stun:{authority}")
}

pub fn safe_signaling_endpoint(input: &str) -> String {
    let input = input.trim();
    let Some(rest) = input.strip_prefix("ws://") else {
        return "(endereço inválido)".to_owned();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.is_empty() {
        "(endereço inválido)".to_owned()
    } else {
        format!("ws://{authority}")
    }
}

impl<'a> MakeWriter<'a> for SharedLogWriter {
    type Writer = LogLine;

    fn make_writer(&'a self) -> Self::Writer {
        LogLine {
            writer: self.clone(),
            bytes: Vec::with_capacity(512),
        }
    }
}

impl Write for LogLine {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LogLine {
    fn drop(&mut self) {
        let result = self
            .writer
            .sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write_line(&self.bytes);
        if let Err(error) = result {
            *self
                .writer
                .last_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
        }
    }
}

impl RollingSink {
    fn open(directory: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        prune_logs(&directory);
        let mut sink = Self {
            directory,
            current_date: None,
            file: None,
        };
        sink.rotate_if_needed()?;
        Ok(sink)
    }

    fn write_line(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.rotate_if_needed()?;
        if let Some(file) = &mut self.file {
            file.write_all(bytes)?;
            if !bytes.ends_with(b"\n") {
                file.write_all(b"\n")?;
            }
            file.flush()?;
        }
        Ok(())
    }

    fn rotate_if_needed(&mut self) -> io::Result<()> {
        let date = date_key(OffsetDateTime::now_utc().date());
        if self.current_date.as_deref() == Some(&date) {
            return Ok(());
        }
        let path = self.directory.join(format!("{LOG_FILE_PREFIX}{date}.log"));
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        self.current_date = Some(date);
        self.file = Some(file);
        prune_logs(&self.directory);
        Ok(())
    }
}

fn open_log_directory() -> Result<(PathBuf, Option<String>, RollingSink), (String, String)> {
    let fallback = std::env::temp_dir()
        .join(LOG_FOLDER_NAME)
        .join("logs-fallback");

    let primary_result = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(|value| {
            let primary = PathBuf::from(value).join(LOG_FOLDER_NAME).join("logs");
            RollingSink::open(primary.clone()).map(|sink| (primary, sink))
        });

    match primary_result {
        Some(Ok((primary, sink))) => Ok((primary, None, sink)),
        primary_result => {
            let primary_error = primary_result.and_then(Result::err).map_or_else(
                || "A variável LOCALAPPDATA não está definida.".to_owned(),
                |error| error.to_string(),
            );
            match RollingSink::open(fallback.clone()) {
                Ok(sink) => Ok((
                    fallback,
                    Some(
                        "Pasta principal de logs indisponível; gravando em pasta temporária."
                            .to_owned(),
                    ),
                    sink,
                )),
                Err(fallback_error) => Err((primary_error, fallback_error.to_string())),
            }
        }
    }
}

fn date_key(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

fn log_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "log")
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(LOG_FILE_PREFIX))
        {
            files.push(path);
        }
    }
    Ok(files)
}

fn prune_logs(directory: &Path) {
    let cutoff = date_key(OffsetDateTime::now_utc().date() - Duration::days(RETENTION_DAYS - 1));
    let Ok(files) = log_files(directory) else {
        return;
    };
    for file in files {
        if let Some(name) = file.file_name().and_then(|name| name.to_str()) {
            let date = name
                .strip_prefix(LOG_FILE_PREFIX)
                .and_then(|name| name.strip_suffix(".log"));
            if date.is_some_and(|date| date.len() == 10 && date < cutoff.as_str()) {
                let _ = fs::remove_file(file);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticSnapshot, LOG_FILE_PREFIX, LoggingState, RollingSink, date_key, prune_logs,
        safe_signaling_endpoint,
    };
    use std::fs;
    use time::{Duration, OffsetDateTime};

    #[test]
    fn date_key_is_sortable_iso_date() {
        assert_eq!(
            date_key(OffsetDateTime::from_unix_timestamp(0).unwrap().date()),
            "1970-01-01"
        );
    }

    #[test]
    fn rolling_sink_persists_utf8_log_lines() {
        let directory = std::env::temp_dir().join(format!(
            "p2p-log-write-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let mut sink = RollingSink::open(directory.clone()).unwrap();
        sink.write_line("evento de teste: captura iniciada".as_bytes())
            .unwrap();
        drop(sink);

        let current = directory.join(format!(
            "{LOG_FILE_PREFIX}{}.log",
            date_key(OffsetDateTime::now_utc().date())
        ));
        assert_eq!(
            fs::read_to_string(current).unwrap(),
            "evento de teste: captura iniciada\n"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn signaling_export_keeps_endpoint_but_removes_userinfo_path_and_query() {
        assert_eq!(
            safe_signaling_endpoint("ws://user:secret@192.168.1.25:9000/ROOM-CODE?token=secret"),
            "ws://192.168.1.25:9000"
        );
    }

    #[test]
    fn prune_logs_removes_only_logs_older_than_fourteen_days() {
        let directory = std::env::temp_dir().join(format!("p2p-log-prune-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let today = OffsetDateTime::now_utc().date();
        let old = date_key(today - Duration::days(15));
        let retained = date_key(today - Duration::days(13));
        let old_path = directory.join(format!("{LOG_FILE_PREFIX}{old}.log"));
        let retained_path = directory.join(format!("{LOG_FILE_PREFIX}{retained}.log"));
        let unrelated_path = directory.join("notes.log");
        fs::write(&old_path, "old").unwrap();
        fs::write(&retained_path, "recent").unwrap();
        fs::write(&unrelated_path, "keep").unwrap();

        prune_logs(&directory);

        assert!(!old_path.exists());
        assert!(retained_path.exists());
        assert!(unrelated_path.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn export_contains_network_diagnostics_and_retained_logs() {
        let directory = std::env::temp_dir().join(format!(
            "p2p-log-export-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let daily_log = directory.join(format!("{LOG_FILE_PREFIX}2026-09-26.log"));
        fs::write(&daily_log, "room_transition=joined\n").unwrap();
        let export = std::env::temp_dir().join(format!(
            "p2p-diagnostic-{}-{}.log",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let state = LoggingState {
            log_directory: Some(directory.clone()),
            ..LoggingState::default()
        };
        let snapshot = DiagnosticSnapshot {
            room_mode: "local/Radmin".to_owned(),
            signaling_address: "ws://user:secret@192.168.1.25:9000/ROOM-CODE-SECRET?token=secret"
                .to_owned(),
            stun_server: "stun:user:secret@stun.example:3478?transport=udp".to_owned(),
            adapters: vec![("Radmin VPN".to_owned(), "26.1.2.3".to_owned())],
            ..DiagnosticSnapshot::default()
        };

        state.export_to(&export, &snapshot).unwrap();

        let contents = fs::read_to_string(&export).unwrap();
        assert!(contents.contains("Radmin VPN: 26.1.2.3"));
        assert!(contents.contains("room_transition=joined"));
        assert!(contents.contains("ws://192.168.1.25:9000"));
        assert!(contents.contains("stun:stun.example:3478"));
        assert!(!contents.contains("secret"));
        assert!(!contents.contains("ROOM-CODE-SECRET"));
        assert!(!contents.to_ascii_lowercase().contains("token="));
        assert!(!contents.contains("ROOM-CODE-SECRET"));
        let _ = fs::remove_file(export);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn stun_export_redacts_user_information_and_query() {
        assert_eq!(
            super::safe_stun_endpoint("stun:user:secret@stun.example:3478?transport=udp"),
            "stun:stun.example:3478"
        );
    }
}
