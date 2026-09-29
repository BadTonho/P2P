use super::release::{
    is_html, looks_like_html, new_http_client, parse_version, sanitized_request_error,
    validate_manifest, validate_response,
};
use super::*;

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

pub(super) fn download_update(
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
        return Err(
            "O GitHub respondeu com uma página HTML em vez do executável do release.".to_owned(),
        );
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
        return Err(
            "O GitHub respondeu com uma página HTML em vez do executável do release.".to_owned(),
        );
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

pub(super) fn validate_downloaded_file(
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
        return Err("O SHA-256 baixado não corresponde ao valor informado pelo GitHub.".to_owned());
    }
    let mut file = File::open(path)
        .map_err(|error| format!("Não foi possível conferir o executável baixado: {error}"))?;
    let mut signature = [0_u8; 2];
    if file.read_exact(&mut signature).is_err() || signature != *b"MZ" {
        return Err("O arquivo baixado não parece ser um executável Windows.".to_owned());
    }
    let actual_version = read_embedded_build_version(path)?;
    if actual_version != manifest.version {
        return Err(format!(
            "O release anuncia a versao {}, mas o executavel baixado identifica-se como {}. O arquivo nao sera aplicado.",
            manifest.version, actual_version
        ));
    }
    Ok(())
}

pub(super) fn read_embedded_build_version(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| {
        format!("Nao foi possivel abrir o executavel para conferir a versao: {error}")
    })?;
    let mut read_buffer = [0_u8; 64 * 1024];
    let mut pending = Vec::with_capacity(read_buffer.len());
    let mut marker_found = false;

    loop {
        let read = file.read(&mut read_buffer).map_err(|error| {
            format!("Nao foi possivel ler o executavel para conferir a versao: {error}")
        })?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&read_buffer[..read]);

        if !marker_found {
            if let Some(marker_at) = find_subslice(&pending, BUILD_VERSION_MARKER_PREFIX) {
                pending.drain(..marker_at + BUILD_VERSION_MARKER_PREFIX.len());
                marker_found = true;
            } else {
                let keep = BUILD_VERSION_MARKER_PREFIX.len().saturating_sub(1);
                if pending.len() > keep {
                    let remove = pending.len() - keep;
                    pending.drain(..remove);
                }
                continue;
            }
        }

        if let Some(end) = pending.iter().position(|byte| *byte == 0) {
            let version = std::str::from_utf8(&pending[..end])
                .map_err(|_| "O marcador de versao do executavel nao e UTF-8 valido.".to_owned())?;
            parse_version(version)?;
            return Ok(version.to_owned());
        }
        if pending.len() > MAX_BUILD_VERSION_BYTES {
            return Err("O marcador de versao do executavel esta malformado.".to_owned());
        }
    }

    if marker_found {
        Err("O marcador de versao do executavel esta incompleto.".to_owned())
    } else {
        Err(
            "O executavel nao contem o marcador interno de versao; atualizacao recusada."
                .to_owned(),
        )
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::test_support::{executable_stub, manifest, test_directory};
    use sha2::{Digest, Sha256};
    use std::fs;

    #[test]
    fn downloaded_payload_must_match_size_hash_and_windows_executable_signature() {
        let directory = test_directory();
        let path = directory.join("download.exe");
        let bytes = executable_stub("1.0.1");
        fs::write(&path, &bytes).unwrap();
        let mut value = manifest();
        value.size_bytes = bytes.len() as u64;
        value.sha256 = format!("{:x}", Sha256::digest(&bytes));

        assert!(validate_downloaded_file(&path, &value, bytes.len() as u64, &value.sha256).is_ok());
        assert!(validate_downloaded_file(&path, &value, 1, &value.sha256).is_err());
        assert!(
            validate_downloaded_file(&path, &value, bytes.len() as u64, &"0".repeat(64)).is_err()
        );

        let wrong_version = executable_stub("1.0.2");
        fs::write(&path, &wrong_version).unwrap();
        value.size_bytes = wrong_version.len() as u64;
        value.sha256 = format!("{:x}", Sha256::digest(&wrong_version));
        assert!(
            validate_downloaded_file(&path, &value, wrong_version.len() as u64, &value.sha256)
                .unwrap_err()
                .contains("1.0.2")
        );

        fs::write(&path, b"MZ executable without build marker").unwrap();
        value.size_bytes = fs::metadata(&path).unwrap().len();
        let no_marker = fs::read(&path).unwrap();
        value.sha256 = format!("{:x}", Sha256::digest(&no_marker));
        assert!(
            validate_downloaded_file(&path, &value, no_marker.len() as u64, &value.sha256).is_err()
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn build_version_marker_must_be_present_and_well_formed() {
        let directory = test_directory();
        let path = directory.join("version.exe");
        fs::write(&path, executable_stub("1.2.3")).unwrap();
        assert_eq!(read_embedded_build_version(&path).unwrap(), "1.2.3");

        fs::write(
            &path,
            [b"MZ".as_slice(), BUILD_VERSION_MARKER_PREFIX, b"bad\0"].concat(),
        )
        .unwrap();
        assert!(read_embedded_build_version(&path).is_err());

        fs::write(&path, b"MZ without marker").unwrap();
        assert!(read_embedded_build_version(&path).is_err());
        let _ = fs::remove_dir_all(directory);
    }
}
