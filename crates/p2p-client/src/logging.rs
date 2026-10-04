use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};

use crate::settings::LoggingLevel;
use time::{Date, Duration, OffsetDateTime};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::time::UtcTime;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;

const LOG_FOLDER_NAME: &str = "P2P-Voz-e-tela";
const LOG_FILE_PREFIX: &str = "p2p-";
const RETENTION_DAYS: i64 = 14;
const MAX_TRACK_SSRCS: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SrtpTrackContext {
    route: Option<String>,
    selected_ice_pair: Option<String>,
}

#[derive(Default)]
struct SrtpContextRegistry {
    sessions_by_ssrc: HashMap<u32, HashMap<u64, SrtpTrackContext>>,
    ssrc_order: VecDeque<u32>,
}

static SRTP_CONTEXTS: OnceLock<Mutex<SrtpContextRegistry>> = OnceLock::new();

fn srtp_context_registry() -> &'static Mutex<SrtpContextRegistry> {
    SRTP_CONTEXTS.get_or_init(|| Mutex::new(SrtpContextRegistry::default()))
}

pub(super) fn register_srtp_track_context(
    ssrc: u32,
    session_id: u64,
    route: Option<&str>,
    selected_ice_pair: Option<&str>,
) {
    let mut registry = srtp_context_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !registry.sessions_by_ssrc.contains_key(&ssrc) {
        registry.ssrc_order.push_back(ssrc);
        if registry.ssrc_order.len() > MAX_TRACK_SSRCS
            && let Some(oldest) = registry.ssrc_order.pop_front()
        {
            registry.sessions_by_ssrc.remove(&oldest);
        }
    }
    registry.sessions_by_ssrc.entry(ssrc).or_default().insert(
        session_id,
        SrtpTrackContext {
            route: route.map(str::to_owned),
            selected_ice_pair: selected_ice_pair.map(str::to_owned),
        },
    );
}

pub(super) fn remove_srtp_contexts_for_session(session_id: u64) {
    let mut registry = srtp_context_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.sessions_by_ssrc.retain(|_, sessions| {
        sessions.remove(&session_id);
        !sessions.is_empty()
    });
    let active_ssrcs = registry
        .sessions_by_ssrc
        .keys()
        .copied()
        .collect::<HashSet<_>>();
    registry
        .ssrc_order
        .retain(|ssrc| active_ssrcs.contains(ssrc));
}

fn describe_srtp_context(registry: &SrtpContextRegistry, ssrc: Option<u32>) -> String {
    let Some(ssrc) = ssrc else {
        return "sessão desconhecida, SSRC desconhecido, rota desconhecida, par ICE desconhecido"
            .to_owned();
    };
    let Some(sessions) = registry.sessions_by_ssrc.get(&ssrc) else {
        return format!(
            "sessão desconhecida, SSRC {ssrc}, rota desconhecida, par ICE desconhecido"
        );
    };
    if sessions.len() != 1 {
        return format!(
            "sessão desconhecida (SSRC associado a {} sessões), SSRC {ssrc}, rota desconhecida, par ICE desconhecido",
            sessions.len()
        );
    }
    let Some((session_id, context)) = sessions.iter().next() else {
        return format!(
            "sessão desconhecida, SSRC {ssrc}, rota desconhecida, par ICE desconhecido"
        );
    };
    format!(
        "sessão {session_id}, SSRC {ssrc}, rota {}, par ICE {}",
        context.route.as_deref().unwrap_or("desconhecida"),
        context
            .selected_ice_pair
            .as_deref()
            .unwrap_or("desconhecido")
    )
}

fn current_srtp_context(ssrc: Option<u32>) -> String {
    let registry = srtp_context_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    describe_srtp_context(&registry, ssrc)
}

type FilterReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;

#[derive(Default)]
pub struct LoggingState {
    log_directory: Option<PathBuf>,
    current_log_file: Option<PathBuf>,
    last_write_error: Option<Arc<Mutex<Option<String>>>>,
    sink: Option<Arc<Mutex<RollingSink>>>,
    writer_sender: Option<mpsc::SyncSender<LogMessage>>,
    filter_reload: Option<FilterReloadHandle>,
    level: LoggingLevel,
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
    pub current_log_file: String,
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
    sender: Option<mpsc::SyncSender<LogMessage>>,
    dropped_counter: Arc<AtomicU64>,
    sink: Arc<Mutex<RollingSink>>,
    last_error: Arc<Mutex<Option<String>>>,
}

enum LogMessage {
    Line(Vec<u8>),
    Flush(mpsc::SyncSender<()>),
}

struct RollingSink {
    path: Option<PathBuf>,
    file: Option<File>,
    error_path: Option<PathBuf>,
    error_file: Option<File>,
    errors_enabled: bool,
    stderr_fallback: bool,
    duplicate_packet_warnings: HashMap<Option<u32>, u64>,
    duplicate_packet_warning_order: VecDeque<Option<u32>>,
}

impl RollingSink {
    fn inactive() -> Self {
        Self {
            path: None,
            file: None,
            error_path: None,
            error_file: None,
            errors_enabled: true,
            stderr_fallback: false,
            duplicate_packet_warnings: HashMap::new(),
            duplicate_packet_warning_order: VecDeque::new(),
        }
    }
}

fn log_filter(level: LoggingLevel) -> EnvFilter {
    match level {
        LoggingLevel::Disabled => EnvFilter::new("off"),
        LoggingLevel::WarningsAndErrors => EnvFilter::new("warn"),
        LoggingLevel::Detailed => EnvFilter::new("info,p2p_client=debug"),
    }
}

fn should_open_log_file(level: LoggingLevel) -> bool {
    level != LoggingLevel::Disabled
}

fn open_log_file_if_enabled(level: LoggingLevel, open: impl FnOnce()) {
    if should_open_log_file(level) {
        open();
    }
}

fn is_duplicate_packet_warning(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    (line.contains("warn") || line.contains("warning")) && line.contains("duplicat")
}

fn duplicate_packet_ssrc(line: &str) -> Option<u32> {
    let value = line.split_once("srtp ssrc=")?.1;
    let digits = value
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    digits.parse().ok()
}

fn increment_duplicate_warning_count(
    counts: &mut HashMap<Option<u32>, u64>,
    order: &mut VecDeque<Option<u32>>,
    ssrc: Option<u32>,
) -> u64 {
    if !counts.contains_key(&ssrc) {
        order.push_back(ssrc);
        if order.len() > 64
            && let Some(oldest) = order.pop_front()
        {
            counts.remove(&oldest);
        }
    }
    let count = counts.entry(ssrc).or_default();
    *count = count.saturating_add(1);
    *count
}

fn install_subscriber<S>(subscriber: S) -> Result<(), String>
where
    S: tracing::Subscriber + Send + Sync + 'static,
{
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|error| format!("não foi possível ativar o subscriber: {error}"))?;
    tracing_log::LogTracer::builder()
        .ignore_crate("turn_server")
        .init()
        .map_err(|error| format!("não foi possível ativar o adaptador de logs: {error}"))
}

struct LogLine {
    writer: SharedLogWriter,
    bytes: Vec<u8>,
}

impl LoggingState {
    pub fn initialize(level: LoggingLevel) -> Self {
        let sink = Arc::new(Mutex::new(RollingSink::inactive()));
        let last_error = Arc::new(Mutex::new(None));
        let (sender, receiver) = mpsc::sync_channel(4096);
        let dropped_counter = Arc::new(AtomicU64::new(0));

        let worker_sink = Arc::clone(&sink);
        let worker_error = Arc::clone(&last_error);
        let worker_dropped = Arc::clone(&dropped_counter);
        std::thread::Builder::new()
            .name("p2p-log-writer".to_owned())
            .spawn(move || {
                log_writer_worker(receiver, worker_sink, worker_error, worker_dropped);
            })
            .ok();

        let writer = SharedLogWriter {
            sender: Some(sender.clone()),
            dropped_counter,
            sink: Arc::clone(&sink),
            last_error: Arc::clone(&last_error),
        };

        let (filter_layer, filter_reload) = reload::Layer::new(log_filter(level));
        let format_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_timer(UtcTime::rfc_3339())
            .with_writer(writer);
        let subscriber = tracing_subscriber::registry()
            .with(filter_layer)
            .with(format_layer);

        let mut state = Self {
            log_directory: existing_log_directory(),
            current_log_file: None,
            last_write_error: Some(last_error),
            sink: Some(sink),
            writer_sender: Some(sender),
            filter_reload: Some(filter_reload),
            level,
            startup_message: None,
            export_message: None,
        };
        open_log_file_if_enabled(level, || state.activate_log_file());
        if let Err(error) = install_subscriber(subscriber) {
            state.filter_reload = None;
            state.startup_message = Some(format!(
                "N\u{00e3}o foi poss\u{00ed}vel ativar o logger: {error}"
            ));
        }
        tracing::info!(
            app_version = env!("CARGO_PKG_VERSION"),
            build_version_marker = crate::update::build_version_marker(),
            os = std::env::consts::OS,
            architecture = std::env::consts::ARCH,
            log_directory = state.actual_log_directory_label(),
            log_file = state.current_log_file_label(),
            "Aplicativo iniciado; logger persistente pronto"
        );
        state
    }

    pub fn set_level(&mut self, level: LoggingLevel) {
        self.set_level_with(level, open_log_directory);
    }

    pub fn set_errors_enabled(&mut self, enabled: bool) {
        if let Some(sink) = &self.sink {
            sink.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .errors_enabled = enabled;
        }
    }

    fn set_level_with(
        &mut self,
        level: LoggingLevel,
        open_log_directory: impl FnOnce() -> Result<
            (PathBuf, Option<String>, RollingSink),
            (String, String),
        >,
    ) {
        if self.level == level {
            return;
        }
        if level != LoggingLevel::Disabled && self.current_log_file.is_none() {
            self.activate_log_file_with(open_log_directory);
        }
        if let Some(filter_reload) = &self.filter_reload
            && let Err(error) = filter_reload.reload(log_filter(level))
        {
            self.startup_message = Some(format!(
                "N\u{00e3}o foi poss\u{00ed}vel alterar o n\u{00ed}vel dos logs: {error}"
            ));
            return;
        }
        self.level = level;
    }

    fn activate_log_file(&mut self) {
        self.activate_log_file_with(open_log_directory);
    }

    fn activate_log_file_with(
        &mut self,
        open_log_directory: impl FnOnce() -> Result<
            (PathBuf, Option<String>, RollingSink),
            (String, String),
        >,
    ) {
        match open_log_directory() {
            Ok((directory, startup_message, rolling_sink)) => {
                self.current_log_file = rolling_sink.path.clone();
                self.log_directory = Some(directory);
                self.startup_message = startup_message;
                if let Some(sink) = &self.sink {
                    *sink
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = rolling_sink;
                }
            }
            Err((primary_error, fallback_error)) => {
                self.startup_message = Some(format!(
                    "N\u{00e3}o foi poss\u{00ed}vel iniciar os logs persistentes. Pasta principal: {primary_error}. Pasta tempor\u{00e1}ria: {fallback_error}."
                ));
                if let Some(sink) = &self.sink {
                    sink.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .stderr_fallback = true;
                }
            }
        }
    }

    pub fn status_label(&self) -> String {
        match &self.startup_message {
            Some(message) => format!("{}; {message}", self.level.label()),
            None => self.level.label().to_owned(),
        }
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
        output.push_str(&format!(
            "Arquivo desta execução: {}\n",
            snapshot.current_log_file
        ));
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
        if self.level != LoggingLevel::Disabled {
            self.flush();
            output.push_str("\n========== LOG DESTA EXECUCAO ==========\n");
        }

        if self.level == LoggingLevel::Disabled {
            output.push_str("Logs de eventos desativados; esta exportacao contem apenas o relatorio de diagnostico.\n");
        } else if let Some(current_log_file) = &self.current_log_file {
            output.push_str(&fs::read_to_string(current_log_file)?);
            if !output.ends_with('\n') {
                output.push('\n');
            }
        } else {
            output.push_str("Logger persistente indisponivel nesta execucao.\n");
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

    pub fn flush(&self) {
        if let Some(writer_sender) = &self.writer_sender {
            let (tx, rx) = mpsc::sync_channel(1);
            if writer_sender.send(LogMessage::Flush(tx)).is_ok() {
                let _ = rx.recv_timeout(std::time::Duration::from_millis(500));
            }
        } else if let Some(sink) = &self.sink {
            let mut guard = sink
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = guard.flush_files();
        }
    }

    pub fn log_directory(&self) -> Option<&Path> {
        self.log_directory.as_deref()
    }

    pub fn actual_log_directory_label(&self) -> String {
        self.log_directory
            .as_ref()
            .map(|directory| directory.display().to_string())
            .unwrap_or_else(|| {
                if self.level == LoggingLevel::Disabled {
                    "(logs desativados; nenhuma pasta criada)".to_owned()
                } else {
                    "(indisponivel)".to_owned()
                }
            })
    }

    pub fn current_log_file(&self) -> Option<&Path> {
        self.current_log_file.as_deref()
    }

    pub fn errors_log_file(&self) -> Option<PathBuf> {
        self.log_directory
            .as_ref()
            .map(|d| d.join("p2p-errors.log"))
    }

    pub fn current_log_file_label(&self) -> String {
        self.current_log_file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| {
                if self.level == LoggingLevel::Disabled {
                    "(desativados nesta execucao)".to_owned()
                } else {
                    "(indisponivel)".to_owned()
                }
            })
    }

    pub fn open_log_directory(&self) -> io::Result<()> {
        let Some(directory) = &self.log_directory else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                if self.level == LoggingLevel::Disabled {
                    "Logs desativados; nenhuma pasta foi criada."
                } else {
                    "A pasta de logs nao esta disponivel."
                },
            ));
        };

        #[cfg(windows)]
        {
            std::process::Command::new("explorer.exe")
                .arg(directory)
                .spawn()
                .map(|_| ())
        }
        #[cfg(not(windows))]
        {
            let _ = directory;
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Abrir a pasta de logs esta disponivel apenas no Windows.",
            ))
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
        if self.bytes.is_empty() {
            return;
        }
        if let Some(sender) = &self.writer.sender {
            let line = std::mem::take(&mut self.bytes);
            if let Err(mpsc::TrySendError::Full(_)) = sender.try_send(LogMessage::Line(line)) {
                self.writer.dropped_counter.fetch_add(1, Ordering::Relaxed);
            }
        } else {
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
}

impl RollingSink {
    fn open(directory: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        prune_logs(&directory);
        let path = directory.join(session_log_filename(
            OffsetDateTime::now_utc(),
            std::process::id(),
        ));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let error_path = directory.join("p2p-errors.log");
        Ok(Self {
            path: Some(path),
            file: Some(file),
            error_path: Some(error_path),
            error_file: None,
            errors_enabled: true,
            stderr_fallback: false,
            duplicate_packet_warnings: HashMap::new(),
            duplicate_packet_warning_order: VecDeque::new(),
        })
    }

    fn write_line(&mut self, bytes: &[u8]) -> io::Result<()> {
        let line = String::from_utf8_lossy(bytes);
        if is_duplicate_packet_warning(&line) {
            let ssrc = duplicate_packet_ssrc(&line);
            let count = increment_duplicate_warning_count(
                &mut self.duplicate_packet_warnings,
                &mut self.duplicate_packet_warning_order,
                ssrc,
            );
            if count != 1 && !count.is_power_of_two() {
                return Ok(());
            }

            let mut aggregated = bytes.to_vec();
            let newline = aggregated.pop().filter(|byte| *byte == b'\n');
            aggregated.extend_from_slice(
                format!(
                    " [avisos SRTP duplicados agregados: {count} ocorrencias; {}]",
                    current_srtp_context(ssrc)
                )
                .as_bytes(),
            );
            if let Some(newline) = newline {
                aggregated.push(newline);
            }
            return self.write_raw_line(&aggregated);
        }
        self.write_raw_line(bytes)
    }

    fn write_raw_line(&mut self, bytes: &[u8]) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.write_all(bytes)?;
            if !bytes.ends_with(b"\n") {
                file.write_all(b"\n")?;
            }
            file.flush()?;
        } else if self.stderr_fallback {
            let mut stderr = io::stderr().lock();
            stderr.write_all(bytes)?;
            if !bytes.ends_with(b"\n") {
                stderr.write_all(b"\n")?;
            }
            stderr.flush()?;
        }

        if self.errors_enabled && is_error_line(bytes) {
            if self.error_file.is_none()
                && let Some(error_path) = &self.error_path
            {
                self.error_file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(error_path)
                    .ok();
            }
            if let Some(error_file) = &mut self.error_file {
                let _ = error_file.write_all(bytes);
                if !bytes.ends_with(b"\n") {
                    let _ = error_file.write_all(b"\n");
                }
                let _ = error_file.flush();
            }
        }
        Ok(())
    }

    fn flush_files(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        if let Some(error_file) = &mut self.error_file {
            let _ = error_file.flush();
        }
        Ok(())
    }
}

fn log_writer_worker(
    receiver: mpsc::Receiver<LogMessage>,
    sink: Arc<Mutex<RollingSink>>,
    last_error: Arc<Mutex<Option<String>>>,
    dropped_counter: Arc<AtomicU64>,
) {
    while let Ok(msg) = receiver.recv() {
        match msg {
            LogMessage::Line(bytes) => {
                let dropped = dropped_counter.swap(0, Ordering::Relaxed);
                let mut guard = sink
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if dropped > 0 {
                    let notice = format!(
                        "[AVISO: {dropped} mensagens de log foram descartadas devido a fila cheia]\n"
                    );
                    let _ = guard.write_line(notice.as_bytes());
                }
                let result = guard.write_line(&bytes);
                if let Err(error) = result {
                    *last_error
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(error.to_string());
                }
            }
            LogMessage::Flush(ack) => {
                let dropped = dropped_counter.swap(0, Ordering::Relaxed);
                let mut guard = sink
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if dropped > 0 {
                    let notice = format!(
                        "[AVISO: {dropped} mensagens de log foram descartadas devido a fila cheia]\n"
                    );
                    let _ = guard.write_line(notice.as_bytes());
                }
                let _ = guard.flush_files();
                let _ = ack.send(());
            }
        }
    }
    let mut guard = sink
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dropped = dropped_counter.swap(0, Ordering::Relaxed);
    if dropped > 0 {
        let notice =
            format!("[AVISO: {dropped} mensagens de log foram descartadas devido a fila cheia]\n");
        let _ = guard.write_line(notice.as_bytes());
    }
    let _ = guard.flush_files();
}

fn is_error_line(bytes: &[u8]) -> bool {
    let line = String::from_utf8_lossy(bytes);
    line.contains("ERROR") || line.contains("error:")
}

#[cfg(test)]
mod filter_tests {
    use super::{
        SrtpContextRegistry, SrtpTrackContext, describe_srtp_context, duplicate_packet_ssrc,
        increment_duplicate_warning_count, is_duplicate_packet_warning, log_filter,
        register_srtp_track_context, remove_srtp_contexts_for_session, srtp_context_registry,
    };
    use crate::settings::LoggingLevel;
    use std::collections::{HashMap, VecDeque};

    #[test]
    fn configured_log_levels_map_to_the_expected_filters() {
        assert_eq!(log_filter(LoggingLevel::Disabled).to_string(), "off");
        assert_eq!(
            log_filter(LoggingLevel::WarningsAndErrors).to_string(),
            "warn"
        );
        let detailed = log_filter(LoggingLevel::Detailed).to_string();
        assert!(detailed.contains("p2p_client=debug"));
        assert!(detailed.contains("info"));
    }

    #[test]
    fn only_warning_lines_about_duplicates_are_aggregated() {
        assert!(is_duplicate_packet_warning(
            "WARN rtc_srtp packet duplicated"
        ));
        assert!(!is_duplicate_packet_warning("DEBUG packet duplicated"));
        assert!(!is_duplicate_packet_warning("WARN ICE candidate failed"));
    }

    #[test]
    fn duplicate_srtp_warning_extracts_ssrc_for_session_correlation() {
        assert_eq!(
            duplicate_packet_ssrc(
                "WARN rtc::peer_connection::handler: SrtpHandler.handle_read got error: srtp ssrc=177936076 index=39252: duplicated"
            ),
            Some(177_936_076)
        );
        assert_eq!(duplicate_packet_ssrc("WARN packet duplicated"), None);
    }

    #[test]
    fn detects_error_lines_properly() {
        use super::is_error_line;
        assert!(is_error_line(
            b"2026-10-04T00:00:00Z ERROR connection failed"
        ));
        assert!(is_error_line(b"Failed with error: invalid packet"));
        assert!(!is_error_line(
            b"2026-10-04T00:00:00Z INFO connection success"
        ));
        assert!(!is_error_line(b"2026-10-04T00:00:00Z WARN packet delayed"));
    }

    #[test]
    fn duplicate_warning_counts_are_isolated_by_ssrc() {
        let mut counts = HashMap::new();
        let mut order = VecDeque::new();
        assert_eq!(
            increment_duplicate_warning_count(&mut counts, &mut order, Some(10)),
            1
        );
        assert_eq!(
            increment_duplicate_warning_count(&mut counts, &mut order, Some(20)),
            1
        );
        assert_eq!(
            increment_duplicate_warning_count(&mut counts, &mut order, Some(10)),
            2
        );
    }

    #[test]
    fn duplicate_warning_context_reports_session_route_and_selected_pair_only_when_known() {
        let mut registry = SrtpContextRegistry::default();
        registry.sessions_by_ssrc.insert(
            77,
            HashMap::from([(
                1234,
                SrtpTrackContext {
                    route: Some("Direto (P2P)".to_owned()),
                    selected_ice_pair: Some("local 192.168.1.2 -> remoto 192.168.1.3".to_owned()),
                },
            )]),
        );
        let known = describe_srtp_context(&registry, Some(77));
        assert!(known.contains("sessão 1234"));
        assert!(known.contains("rota Direto (P2P)"));
        assert!(known.contains("par ICE local 192.168.1.2 -> remoto 192.168.1.3"));

        let unknown = describe_srtp_context(&registry, Some(88));
        assert!(unknown.contains("rota desconhecida"));
        assert!(unknown.contains("par ICE desconhecido"));
        let ambiguous = describe_srtp_context(
            &SrtpContextRegistry {
                sessions_by_ssrc: HashMap::from([(
                    77,
                    HashMap::from([
                        (1, SrtpTrackContext::default()),
                        (2, SrtpTrackContext::default()),
                    ]),
                )]),
                ..SrtpContextRegistry::default()
            },
            Some(77),
        );
        assert!(ambiguous.contains("sessão desconhecida"));
        assert!(ambiguous.contains("par ICE desconhecido"));
    }

    #[test]
    fn srtp_context_registration_updates_and_removes_session_correlation() {
        let ssrc = u32::MAX - 101;
        let session_id = u64::MAX - 201;
        register_srtp_track_context(ssrc, session_id, None, None);
        let registered = srtp_context_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            describe_srtp_context(&registered, Some(ssrc))
                .contains(&format!("sessão {session_id}"))
        );
        drop(registered);

        register_srtp_track_context(
            ssrc,
            session_id,
            Some("Retransmitido (TURN)"),
            Some("ICE TURN local -> remoto"),
        );
        let updated = srtp_context_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let summary = describe_srtp_context(&updated, Some(ssrc));
        assert!(summary.contains("rota Retransmitido (TURN)"));
        assert!(summary.contains("par ICE ICE TURN local -> remoto"));
        drop(updated);

        remove_srtp_contexts_for_session(session_id);
        let removed = srtp_context_registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(describe_srtp_context(&removed, Some(ssrc)).contains("sessão desconhecida"));
    }
}

fn existing_log_directory() -> Option<PathBuf> {
    let primary = std::env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .map(|value| PathBuf::from(value).join(LOG_FOLDER_NAME).join("logs"));
    if let Some(directory) = primary.filter(|directory| directory.is_dir()) {
        return Some(directory);
    }
    let fallback = std::env::temp_dir()
        .join(LOG_FOLDER_NAME)
        .join("logs-fallback");
    fallback.is_dir().then_some(fallback)
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

fn session_log_filename(now: OffsetDateTime, process_id: u32) -> String {
    let subsecond_nanos = now.unix_timestamp_nanos().rem_euclid(1_000_000_000);
    format!(
        "{LOG_FILE_PREFIX}{}-{:02}{:02}{:02}-{:09}-{process_id}.log",
        date_key(now.date()),
        now.hour(),
        now.minute(),
        now.second(),
        subsecond_nanos
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
                .and_then(|name| name.get(..10));
            if date.is_some_and(|date| date < cutoff.as_str()) {
                let _ = fs::remove_file(file);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DiagnosticSnapshot, LOG_FILE_PREFIX, LogMessage, LoggingState, RollingSink,
        SharedLogWriter, date_key, log_filter, log_writer_worker, open_log_file_if_enabled,
        prune_logs, safe_signaling_endpoint,
    };
    use crate::settings::LoggingLevel;
    use std::fs;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex};
    use time::{Duration, OffsetDateTime};
    use tracing_subscriber::layer::SubscriberExt;

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
        let current_log = sink.path.clone().unwrap();
        sink.write_line("evento de teste: captura iniciada".as_bytes())
            .unwrap();
        drop(sink);

        assert_eq!(
            fs::read_to_string(current_log).unwrap(),
            "evento de teste: captura iniciada\n"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn disabled_start_does_not_open_or_create_a_log_directory() {
        let directory = std::env::temp_dir().join(format!(
            "p2p-log-disabled-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let mut opened = false;
        open_log_file_if_enabled(LoggingLevel::Disabled, || {
            opened = true;
            fs::create_dir_all(&directory).unwrap();
        });
        assert!(!opened);
        assert!(!directory.exists());
        open_log_file_if_enabled(LoggingLevel::WarningsAndErrors, || {
            opened = true;
            fs::create_dir_all(&directory).unwrap();
        });
        assert!(opened);
        assert!(directory.exists());
        fs::remove_dir_all(&directory).unwrap();
        assert!(!directory.exists());
    }

    #[test]
    fn enabling_logs_mid_session_creates_a_file_and_keeps_previous_logs() {
        let directory = std::env::temp_dir().join(format!(
            "p2p-log-enable-later-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let previous_log = directory.join(format!(
            "{LOG_FILE_PREFIX}{}-120000-123456789-1.log",
            date_key(OffsetDateTime::now_utc().date())
        ));
        fs::write(&previous_log, "previous session\n").unwrap();
        let mut state = LoggingState {
            log_directory: Some(directory.clone()),
            sink: Some(Arc::new(Mutex::new(RollingSink::inactive()))),
            level: LoggingLevel::Disabled,
            ..LoggingState::default()
        };

        state.set_level_with(LoggingLevel::Disabled, || {
            panic!("disabled must not open logs")
        });
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
        assert!(previous_log.exists());

        state.set_level_with(LoggingLevel::WarningsAndErrors, || {
            RollingSink::open(directory.clone())
                .map(|sink| (directory.clone(), None, sink))
                .map_err(|error| (error.to_string(), error.to_string()))
        });

        assert!(state.current_log_file().is_some_and(|path| path.exists()));
        assert!(previous_log.exists());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn log_filter_changes_apply_to_events_immediately() {
        let directory = std::env::temp_dir().join(format!(
            "p2p-log-levels-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let sink = Arc::new(Mutex::new(RollingSink::open(directory.clone()).unwrap()));
        let log_path = sink.lock().unwrap().path.clone().unwrap();
        let last_error = Arc::new(Mutex::new(None));
        let writer = SharedLogWriter {
            sender: None,
            dropped_counter: Arc::new(AtomicU64::new(0)),
            sink: Arc::clone(&sink),
            last_error: Arc::clone(&last_error),
        };
        let (filter_layer, filter_reload) =
            tracing_subscriber::reload::Layer::new(log_filter(LoggingLevel::WarningsAndErrors));
        let format_layer = tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(writer);
        let subscriber = tracing_subscriber::registry()
            .with(filter_layer)
            .with(format_layer);
        let mut state = LoggingState {
            log_directory: Some(directory.clone()),
            current_log_file: Some(log_path.clone()),
            last_write_error: Some(last_error),
            sink: Some(sink),
            filter_reload: Some(filter_reload),
            level: LoggingLevel::WarningsAndErrors,
            ..LoggingState::default()
        };

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("info_before_detail");
            tracing::warn!("warning_kept");
            tracing::error!("error_kept");
            state.set_level(LoggingLevel::Detailed);
            tracing::info!("info_after_detail");
            tracing::debug!(target: "p2p_client::test", "debug_after_detail");
            state.set_level(LoggingLevel::Disabled);
            tracing::error!("error_after_disable");
        });

        let contents = fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("warning_kept"));
        assert!(contents.contains("error_kept"));
        assert!(contents.contains("info_after_detail"));
        assert!(contents.contains("debug_after_detail"));
        assert!(!contents.contains("info_before_detail"));
        assert!(!contents.contains("error_after_disable"));
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
        let old_path = directory.join(format!("{LOG_FILE_PREFIX}{old}-120000-123456789-1.log"));
        let retained_path = directory.join(format!(
            "{LOG_FILE_PREFIX}{retained}-120000-123456789-1.log"
        ));
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
    fn export_contains_network_diagnostics_and_only_the_current_session_log() {
        let directory = std::env::temp_dir().join(format!(
            "p2p-log-export-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let previous_log = directory.join(format!(
            "{LOG_FILE_PREFIX}2026-09-26-210000-123456789-41.log"
        ));
        let current_log = directory.join(format!(
            "{LOG_FILE_PREFIX}2026-09-27-210000-123456789-42.log"
        ));
        fs::write(&previous_log, "previous_session_event=true\n").unwrap();
        fs::write(&current_log, "current_session_event=true\n").unwrap();
        let export = std::env::temp_dir().join(format!(
            "p2p-diagnostic-{}-{}.log",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let state = LoggingState {
            log_directory: Some(directory.clone()),
            current_log_file: Some(current_log.clone()),
            ..LoggingState::default()
        };
        let snapshot = DiagnosticSnapshot {
            current_log_file: current_log.display().to_string(),
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
        assert!(contents.contains("current_session_event=true"));
        assert!(!contents.contains("previous_session_event=true"));
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
    fn disabled_export_contains_diagnostics_without_event_history() {
        let directory = std::env::temp_dir().join(format!(
            "p2p-log-export-disabled-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let current_log = directory.join("current.log");
        fs::write(&current_log, "private_event_history=true\n").unwrap();
        let export = std::env::temp_dir().join(format!(
            "p2p-diagnostic-disabled-{}-{}.log",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let state = LoggingState {
            log_directory: Some(directory.clone()),
            current_log_file: Some(current_log),
            level: LoggingLevel::Disabled,
            ..LoggingState::default()
        };

        state
            .export_to(&export, &DiagnosticSnapshot::default())
            .unwrap();

        let contents = fs::read_to_string(&export).unwrap();
        assert!(contents.contains("esta exportacao contem apenas o relatorio de diagnostico"));
        assert!(!contents.contains("private_event_history"));
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

    #[test]
    fn async_log_worker_processes_messages_and_flushes() {
        let directory = std::env::temp_dir().join(format!(
            "p2p-log-async-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        let sink = Arc::new(Mutex::new(RollingSink::open(directory.clone()).unwrap()));
        let last_error = Arc::new(Mutex::new(None));
        let dropped_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (sender, receiver) = std::sync::mpsc::sync_channel(32);

        let worker_sink = Arc::clone(&sink);
        let worker_error = Arc::clone(&last_error);
        let worker_dropped = Arc::clone(&dropped_counter);
        let handle = std::thread::spawn(move || {
            log_writer_worker(receiver, worker_sink, worker_error, worker_dropped);
        });

        sender
            .send(LogMessage::Line(b"async line 1\n".to_vec()))
            .unwrap();
        sender
            .send(LogMessage::Line(b"async line 2\n".to_vec()))
            .unwrap();
        let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
        sender.send(LogMessage::Flush(ack_tx)).unwrap();
        ack_rx.recv().unwrap();

        let log_path = sink.lock().unwrap().path.clone().unwrap();
        let contents = fs::read_to_string(&log_path).unwrap();
        assert!(contents.contains("async line 1"));
        assert!(contents.contains("async line 2"));

        drop(sender);
        handle.join().unwrap();
        let _ = fs::remove_dir_all(directory);
    }
}
