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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST_URL: &str = env!("P2P_UPDATE_MANIFEST_URL");
const MANIFEST_MAX_BYTES: u64 = 64 * 1024;
const UPDATE_MAX_BYTES: u64 = 1024 * 1024 * 1024;
const UPDATE_DIRECTORY: &str = "P2P-Voz-e-tela\\updates";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
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

    pub fn is_configured() -> bool {
        !MANIFEST_URL.trim().is_empty()
    }

    pub fn check(&mut self) {
        let sender = self.channel();
        if !Self::is_configured() {
            let _ = sender.send(UpdateEvent::CheckFinished(Err(
                "Esta compilação não tem um link de atualizações configurado.".to_owned(),
            )));
            return;
        }

        tracing::info!(
            current_version = env!("CARGO_PKG_VERSION"),
            "Verificando atualizações"
        );
        thread::spawn(move || {
            let result = check_for_update(MANIFEST_URL);
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
        thread::spawn(move || download_update(manifest, cancellation, sender));
    }

    pub fn cancel_download(&mut self) {
        if let Some(cancel) = self.cancel_download.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn apply(&mut self, downloaded: PathBuf) {
        let sender = self.channel();
        thread::spawn(move || {
            let result = stage_and_launch(downloaded);
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

fn new_http_client() -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .redirect(Policy::limited(10))
        .build()
        .map_err(|_| "Não foi possível preparar a conexão HTTPS.".to_owned())
}

fn validate_https_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| "O link de atualização não é uma URL válida.".to_owned())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Os links de atualização precisam usar HTTPS.".to_owned());
    }
    Ok(())
}

fn check_for_update(manifest_url: &str) -> Result<Option<UpdateManifest>, String> {
    validate_https_url(manifest_url)?;
    let client = new_http_client()?;
    let response = client
        .get(manifest_url)
        .send()
        .map_err(|error| sanitized_request_error(&error))?;
    let response = validate_response(response, MANIFEST_MAX_BYTES, "manifesto")?;
    if is_html(&response) {
        return Err("O Drive respondeu com uma página HTML em vez do manifesto. Confira o link de download e a permissão 'Qualquer pessoa com o link'.".to_owned());
    }
    let mut bytes = Vec::new();
    response
        .take(MANIFEST_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Não foi possível ler o manifesto de atualização.".to_owned())?;
    if bytes.len() as u64 > MANIFEST_MAX_BYTES {
        return Err("O manifesto de atualização excede o limite de tamanho.".to_owned());
    }
    let manifest = parse_manifest(&bytes)?;

    match compare_versions(&manifest.version, env!("CARGO_PKG_VERSION"))? {
        std::cmp::Ordering::Greater => Ok(Some(manifest)),
        std::cmp::Ordering::Equal | std::cmp::Ordering::Less => Ok(None),
    }
}

fn validate_response(
    response: Response,
    max_bytes: u64,
    description: &str,
) -> Result<Response, String> {
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "O servidor de atualizações respondeu com HTTP {status}."
        ));
    }
    if response.url().scheme() != "https" {
        return Err("O redirecionamento do Drive deixou de usar HTTPS.".to_owned());
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(format!("O {description} excede o limite de tamanho."));
    }
    Ok(response)
}

fn is_html(response: &Response) -> bool {
    if response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
    {
        return true;
    }
    false
}

fn looks_like_html(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let prefix = String::from_utf8_lossy(bytes);
    let prefix = prefix.trim_start().to_ascii_lowercase();
    prefix.starts_with("<!doctype html") || prefix.starts_with("<html")
}

fn parse_manifest(bytes: &[u8]) -> Result<UpdateManifest, String> {
    if looks_like_html(bytes) {
        return Err("O Drive respondeu com uma página HTML em vez do manifesto. Confira o link de download e a permissão 'Qualquer pessoa com o link'.".to_owned());
    }
    let json_bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let text = std::str::from_utf8(json_bytes)
        .map_err(|_| "O manifesto de atualização não está em UTF-8 válido.".to_owned())?;
    let manifest: UpdateManifest = serde_json::from_str(text)
        .map_err(|_| "O manifesto de atualização não contém JSON válido.".to_owned())?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &UpdateManifest) -> Result<(), String> {
    parse_version(&manifest.version)?;
    validate_https_url(&manifest.download_url)?;
    if manifest.size_bytes == 0 || manifest.size_bytes > UPDATE_MAX_BYTES {
        return Err("O tamanho informado no manifesto está fora do limite aceito.".to_owned());
    }
    if manifest.sha256.len() != 64 || !manifest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("O SHA-256 do manifesto precisa ter 64 caracteres hexadecimais.".to_owned());
    }
    Ok(())
}

fn parse_version(value: &str) -> Result<[u64; 3], String> {
    let mut parts = value.split('.');
    let parsed = [parts.next(), parts.next(), parts.next()];
    if parts.next().is_some() {
        return Err("A versão deve usar o formato numérico MAJOR.MINOR.PATCH.".to_owned());
    }
    let mut version = [0; 3];
    for (index, part) in parsed.into_iter().enumerate() {
        let Some(part) = part else {
            return Err("A versão deve usar o formato numérico MAJOR.MINOR.PATCH.".to_owned());
        };
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("A versão deve usar o formato numérico MAJOR.MINOR.PATCH.".to_owned());
        }
        version[index] = part
            .parse()
            .map_err(|_| "A versão informada é grande demais.".to_owned())?;
    }
    Ok(version)
}

fn compare_versions(left: &str, right: &str) -> Result<std::cmp::Ordering, String> {
    Ok(parse_version(left)?.cmp(&parse_version(right)?))
}

fn sanitized_request_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "A conexão com o servidor de atualizações expirou.".to_owned()
    } else if error.is_connect() {
        "Não foi possível conectar ao servidor de atualizações.".to_owned()
    } else {
        "Falha ao receber dados do servidor de atualizações.".to_owned()
    }
}

fn update_directory() -> Result<PathBuf, String> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TEMP").map(PathBuf::from))
        .ok_or_else(|| "Não foi possível localizar uma pasta temporária do usuário.".to_owned())?;
    let path = local_app_data.join(UPDATE_DIRECTORY);
    fs::create_dir_all(&path)
        .map_err(|error| format!("Não foi possível preparar a pasta de atualização: {error}"))?;
    Ok(path)
}

fn download_update(
    manifest: UpdateManifest,
    cancellation: Arc<AtomicBool>,
    sender: Sender<UpdateEvent>,
) {
    tracing::info!(version = %manifest.version, size_bytes = manifest.size_bytes, "Download de atualização iniciado");
    let result = download_update_file(&manifest, &cancellation, &sender);
    match result {
        Ok(Some(path)) => {
            tracing::info!(version = %manifest.version, "Download e validação da atualização concluídos");
            let _ = sender.send(UpdateEvent::DownloadFinished { manifest, path });
        }
        Ok(None) => {
            tracing::info!(version = %manifest.version, "Download de atualização cancelado");
            let _ = sender.send(UpdateEvent::DownloadCancelled {
                version: manifest.version,
            });
        }
        Err(error) => {
            tracing::error!(version = %manifest.version, error = %error, "Download de atualização falhou");
            let _ = sender.send(UpdateEvent::DownloadFailed {
                version: manifest.version,
                error,
            });
        }
    }
}

fn download_update_file(
    manifest: &UpdateManifest,
    cancellation: &AtomicBool,
    sender: &Sender<UpdateEvent>,
) -> Result<Option<PathBuf>, String> {
    validate_manifest(manifest)?;
    let client = new_http_client()?;
    let response = client
        .get(&manifest.download_url)
        .send()
        .map_err(|error| sanitized_request_error(&error))?;
    let mut response = validate_response(response, UPDATE_MAX_BYTES, "executável")?;
    if is_html(&response) {
        return Err("O Drive respondeu com uma página HTML. Verifique se o link permite baixar o arquivo sem login e sem confirmação.".to_owned());
    }

    let directory = update_directory()?;
    let part_path = directory.join(format!("p2p-update-{}.part", manifest.version));
    let output_path = directory.join(format!("p2p-update-{}.exe", manifest.version));
    let _ = fs::remove_file(&part_path);
    let _cleanup = RemoveOnDrop(part_path.clone());
    let mut file = File::create(&part_path).map_err(|error| {
        format!("Não foi possível criar o arquivo temporário de atualização: {error}")
    })?;
    let mut hasher = Sha256::new();
    let mut received = 0_u64;
    let mut prefix = Vec::with_capacity(256);
    let mut buffer = [0_u8; 64 * 1024];
    let mut last_progress = Instant::now() - Duration::from_secs(1);

    loop {
        if cancellation.load(Ordering::Relaxed) {
            drop(file);
            let _ = fs::remove_file(&part_path);
            return Ok(None);
        }
        let count = response
            .read(&mut buffer)
            .map_err(|_| "A conexão foi interrompida durante o download.".to_owned())?;
        if count == 0 {
            break;
        }
        received = received.saturating_add(count as u64);
        if received > manifest.size_bytes || received > UPDATE_MAX_BYTES {
            return Err("O executável recebido é maior que o tamanho informado.".to_owned());
        }
        if prefix.len() < 256 {
            let count_to_copy = (256 - prefix.len()).min(count);
            prefix.extend_from_slice(&buffer[..count_to_copy]);
        }
        file.write_all(&buffer[..count])
            .map_err(|error| format!("Não foi possível gravar o download: {error}"))?;
        hasher.update(&buffer[..count]);
        if last_progress.elapsed() >= Duration::from_millis(200) {
            let _ = sender.send(UpdateEvent::DownloadProgress {
                version: manifest.version.clone(),
                received,
            });
            last_progress = Instant::now();
        }
    }
    file.flush()
        .map_err(|error| format!("Não foi possível concluir a gravação do download: {error}"))?;
    drop(file);

    if looks_like_html(&prefix) {
        return Err("O Drive respondeu com uma página HTML. Verifique se o link permite baixar o arquivo sem login e sem confirmação.".to_owned());
    }

    let actual_hash = format!("{:x}", hasher.finalize());
    validate_downloaded_file(&part_path, manifest, received, &actual_hash)?;
    let _ = fs::remove_file(&output_path);
    fs::rename(&part_path, &output_path)
        .map_err(|error| format!("Não foi possível finalizar o arquivo baixado: {error}"))?;
    let _ = sender.send(UpdateEvent::DownloadProgress {
        version: manifest.version.clone(),
        received,
    });
    Ok(Some(output_path))
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn validate_downloaded_file(
    path: &Path,
    manifest: &UpdateManifest,
    received: u64,
    actual_hash: &str,
) -> Result<(), String> {
    if received != manifest.size_bytes {
        return Err(format!(
            "Download incompleto: recebidos {received} de {} bytes.",
            manifest.size_bytes
        ));
    }
    if !actual_hash.eq_ignore_ascii_case(&manifest.sha256) {
        return Err("O SHA-256 baixado não corresponde ao manifesto.".to_owned());
    }
    let mut file = File::open(path)
        .map_err(|error| format!("Não foi possível conferir o executável baixado: {error}"))?;
    let mut signature = [0_u8; 2];
    if file.read_exact(&mut signature).is_err() || signature != *b"MZ" {
        return Err("O arquivo baixado não parece ser um executável Windows.".to_owned());
    }
    Ok(())
}

fn stage_and_launch(downloaded: PathBuf) -> Result<(), String> {
    if !downloaded.is_file() {
        return Err("O executável baixado não foi encontrado.".to_owned());
    }
    let target = std::env::current_exe()
        .map_err(|error| format!("Não foi possível localizar o aplicativo atual: {error}"))?;
    let process_id = std::process::id();
    let staged = append_suffix(&target, &format!(".update-{process_id}"));
    if let Err(error) = fs::copy(&downloaded, &staged) {
        let _ = fs::remove_file(&staged);
        return Err(format!(
            "Não foi possível preparar a atualização ao lado do aplicativo: {error}"
        ));
    }

    let helper = std::env::temp_dir().join("P2P-Voz-e-tela-update-helper.exe");
    if let Err(error) = fs::copy(&target, &helper) {
        let _ = fs::remove_file(&staged);
        return Err(format!(
            "Não foi possível iniciar o aplicador de atualização: {error}"
        ));
    }
    let mut command = Command::new(&helper);
    command
        .arg("--apply-update")
        .arg(&target)
        .arg(&staged)
        .arg(process_id.to_string())
        .arg(&downloaded);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000 | 0x0000_0008);
    }
    if let Err(error) = command.spawn() {
        let _ = fs::remove_file(&staged);
        let _ = fs::remove_file(&helper);
        return Err(format!(
            "Não foi possível iniciar o aplicador de atualização: {error}"
        ));
    }
    tracing::info!("Aplicador de atualização iniciado; aguardando fechamento do aplicativo");
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

pub fn helper_arguments(args: &[OsString]) -> Option<i32> {
    if args
        .first()
        .is_none_or(|arg| arg != OsStr::new("--apply-update"))
    {
        return None;
    }
    Some(run_helper(args))
}

fn run_helper(args: &[OsString]) -> i32 {
    if args.len() != 5 {
        show_update_error("Os argumentos do aplicador de atualização estão incompletos.");
        return 2;
    }
    let target = PathBuf::from(&args[1]);
    let staged = PathBuf::from(&args[2]);
    let downloaded = PathBuf::from(&args[4]);
    let process_id = match args[3].to_string_lossy().parse::<u32>() {
        Ok(value) => value,
        Err(_) => {
            show_update_error("Não foi possível identificar o processo do aplicativo.");
            return 2;
        }
    };
    if let Err(error) = wait_for_process(process_id) {
        let _ = fs::remove_file(&staged);
        show_update_error(&format!(
            "Não foi possível aguardar o fechamento do aplicativo: {error}"
        ));
        let _ = launch_target(&target);
        return 1;
    }

    match replace_and_restart(&target, &staged) {
        Ok(()) => {
            let _ = fs::remove_file(downloaded);
            0
        }
        Err(error) => {
            let _ = fs::remove_file(&staged);
            show_update_error(&error);
            let _ = launch_target(&target);
            1
        }
    }
}

#[cfg(windows)]
fn wait_for_process(process_id: u32) -> io::Result<()> {
    use std::ffi::c_void;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0;
    const INFINITE: u32 = 0xffff_ffff;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, process_id: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, process_id) };
    if handle.is_null() {
        return match io::Error::last_os_error().raw_os_error() {
            Some(87) | Some(1168) => Ok(()),
            _ => Err(io::Error::last_os_error()),
        };
    }
    let result = unsafe { WaitForSingleObject(handle, INFINITE) };
    unsafe { CloseHandle(handle) };
    if result == WAIT_OBJECT_0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn wait_for_process(_process_id: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "o aplicador só está disponível no Windows",
    ))
}

fn replace_files(target: &Path, staged: &Path) -> Result<PathBuf, String> {
    let backup = append_suffix(target, ".backup");
    if backup.exists() {
        fs::remove_file(&backup)
            .map_err(|error| format!("Não foi possível liberar a cópia de recuperação: {error}"))?;
    }
    fs::rename(target, &backup)
        .map_err(|error| format!("Não foi possível guardar a versão atual: {error}"))?;
    if let Err(error) = fs::rename(staged, target) {
        let restore = fs::rename(&backup, target);
        let detail = match restore {
            Ok(()) => format!(
                "Falha ao instalar a atualização; a versão anterior foi restaurada: {error}"
            ),
            Err(restore_error) => format!(
                "Falha ao instalar a atualização ({error}) e ao restaurar a versão anterior ({restore_error}). A cópia de recuperação está ao lado do aplicativo."
            ),
        };
        return Err(detail);
    }
    Ok(backup)
}

fn replace_and_restart(target: &Path, staged: &Path) -> Result<(), String> {
    let backup = replace_files(target, staged)?;
    if let Err(error) = launch_target(target) {
        let _ = fs::remove_file(target);
        let restore = fs::rename(&backup, target);
        let detail = match restore {
            Ok(()) => {
                format!("A nova versão não abriu ({error}); a versão anterior foi restaurada.")
            }
            Err(restore_error) => format!(
                "A nova versão não abriu ({error}) e a restauração também falhou ({restore_error})."
            ),
        };
        return Err(detail);
    }
    tracing::info!("Atualização aplicada e aplicativo reiniciado");
    Ok(())
}

fn launch_target(target: &Path) -> io::Result<()> {
    let mut command = Command::new(target);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000 | 0x0000_0008);
    }
    command.spawn().map(|_| ())
}

#[cfg(windows)]
fn show_update_error(message: &str) {
    use std::ffi::c_void;
    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }
    let text: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let caption: Vec<u16> = "Falha na atualização"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), 0x10) };
}

#[cfg(not(windows))]
fn show_update_error(message: &str) {
    eprintln!("Falha na atualização: {message}");
}

#[cfg(test)]
mod tests {
    use super::{
        UpdateManifest, compare_versions, looks_like_html, parse_manifest, replace_files,
        validate_downloaded_file, validate_https_url, validate_manifest,
    };
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_directory() -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("p2p-updater-test-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn manifest() -> UpdateManifest {
        UpdateManifest {
            version: "0.1.1".to_owned(),
            download_url: "https://drive.google.com/uc?export=download&id=example".to_owned(),
            size_bytes: 123,
            sha256: "a".repeat(64),
        }
    }

    #[test]
    fn compares_numeric_versions_by_component() {
        assert!(compare_versions("0.10.0", "0.9.9").unwrap().is_gt());
        assert!(compare_versions("1.0.0", "1.0.0").unwrap().is_eq());
        assert!(compare_versions("0.1.0", "0.2.0").unwrap().is_lt());
    }

    #[test]
    fn accepts_only_https_links_without_embedded_credentials() {
        assert!(validate_https_url("https://drive.google.com/file?id=x").is_ok());
        assert!(validate_https_url("http://drive.google.com/file?id=x").is_err());
        assert!(validate_https_url("https://user:pass@drive.google.com/file?id=x").is_err());
    }

    #[test]
    fn parses_manifest_with_a_powershell_utf8_bom_and_rejects_drive_html() {
        let body = serde_json::to_vec(&manifest()).unwrap();
        let mut with_bom = vec![0xef, 0xbb, 0xbf];
        with_bom.extend_from_slice(&body);
        assert_eq!(parse_manifest(&with_bom).unwrap(), manifest());
        assert!(looks_like_html(b" <!doctype html><html>Drive error"));
        assert!(parse_manifest(b"<html>login</html>").is_err());
    }

    #[test]
    fn rejects_invalid_manifest_fields() {
        let mut value = manifest();
        assert!(validate_manifest(&value).is_ok());
        value.size_bytes = 0;
        assert!(validate_manifest(&value).is_err());
        value.size_bytes = 123;
        value.sha256 = "not-a-hash".to_owned();
        assert!(validate_manifest(&value).is_err());
        value.sha256 = "a".repeat(64);
        value.version = "0.1".to_owned();
        assert!(validate_manifest(&value).is_err());
    }

    #[test]
    fn replacement_keeps_a_recovery_copy_of_the_old_executable() {
        let directory = test_directory();
        let target = directory.join("app.exe");
        let staged = directory.join("app.exe.update");
        fs::write(&target, b"old version").unwrap();
        fs::write(&staged, b"new version").unwrap();

        let backup = replace_files(&target, &staged).unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new version");
        assert_eq!(fs::read(&backup).unwrap(), b"old version");
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn failed_replacement_restores_the_previous_executable() {
        let directory = test_directory();
        let target = directory.join("app.exe");
        let missing_staged = directory.join("missing.update");
        fs::write(&target, b"old version").unwrap();

        assert!(replace_files(&target, &missing_staged).is_err());

        assert_eq!(fs::read(&target).unwrap(), b"old version");
        assert!(!directory.join("app.exe.backup").exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn downloaded_payload_must_match_size_hash_and_windows_executable_signature() {
        let directory = test_directory();
        let path = directory.join("download.exe");
        let bytes = b"MZfake executable";
        fs::write(&path, bytes).unwrap();
        let mut value = manifest();
        value.size_bytes = bytes.len() as u64;
        value.sha256 = format!("{:x}", Sha256::digest(bytes));

        assert!(validate_downloaded_file(&path, &value, bytes.len() as u64, &value.sha256).is_ok());
        assert!(validate_downloaded_file(&path, &value, 1, &value.sha256).is_err());
        assert!(
            validate_downloaded_file(&path, &value, bytes.len() as u64, &"0".repeat(64)).is_err()
        );

        fs::write(&path, b"<html>not an executable</html>").unwrap();
        assert!(
            validate_downloaded_file(&path, &value, bytes.len() as u64, &value.sha256).is_err()
        );
        let _ = fs::remove_dir_all(directory);
    }
}
