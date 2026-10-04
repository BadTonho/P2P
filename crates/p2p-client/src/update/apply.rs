use super::download::read_embedded_build_version;
use super::release::parse_version;
use super::*;

pub fn stage_and_launch(downloaded: PathBuf, expected_version: String) -> Result<(), String> {
    if !downloaded.is_file() {
        return Err("O executável baixado não foi encontrado.".to_owned());
    }
    let downloaded_version = read_embedded_build_version(&downloaded)?;
    if downloaded_version != expected_version {
        return Err(format!(
            "A atualizacao esperava a versao {expected_version}, mas o arquivo baixado contem {downloaded_version}."
        ));
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

    let helper = helper_executable_path(process_id);
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
        .arg(&downloaded)
        .arg(&expected_version);
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

fn helper_executable_path(process_id: u32) -> PathBuf {
    std::env::temp_dir().join(format!("P2P-Voz-e-tela-update-helper-{process_id}.exe"))
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
    if args.len() != 6 {
        show_update_error("Os argumentos do aplicador de atualização estão incompletos.");
        return 2;
    }
    let target = PathBuf::from(&args[1]);
    let staged = PathBuf::from(&args[2]);
    let downloaded = PathBuf::from(&args[4]);
    let expected_version = args[5].to_string_lossy().into_owned();
    if parse_version(&expected_version).is_err() {
        show_update_error("A versao esperada pelo aplicador de atualizacao e invalida.");
        return 2;
    }
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

    match replace_and_restart(&target, &staged, &expected_version) {
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

pub(super) fn replace_files(target: &Path, staged: &Path) -> Result<PathBuf, String> {
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

fn replace_and_restart(target: &Path, staged: &Path, expected_version: &str) -> Result<(), String> {
    let staged_version = read_embedded_build_version(staged)?;
    if staged_version != expected_version {
        return Err(format!(
            "O arquivo preparado contem a versao {staged_version}, mas era esperada {expected_version}. A versao instalada foi preservada."
        ));
    }

    let backup = replace_files(target, staged)?;
    verify_installed_version_or_restore(target, &backup, expected_version)?;

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

pub(super) fn verify_installed_version_or_restore(
    target: &Path,
    backup: &Path,
    expected_version: &str,
) -> Result<(), String> {
    match read_embedded_build_version(target) {
        Ok(installed_version) if installed_version == expected_version => Ok(()),
        result => {
            let detail = match result {
                Ok(installed_version) => format!(
                    "Apos a substituicao, o executavel identifica-se como {installed_version}, mas era esperada a versao {expected_version}."
                ),
                Err(error) => {
                    format!("Nao foi possivel verificar o executavel apos a substituicao: {error}")
                }
            };
            let _ = fs::remove_file(target);
            match fs::rename(backup, target) {
                Ok(()) => Err(format!(
                    "{detail} A versao anterior foi restaurada; o aplicativo nao sera reiniciado."
                )),
                Err(restore_error) => Err(format!(
                    "{detail} A restauracao falhou ({restore_error}); a copia de recuperacao permanece ao lado do aplicativo."
                )),
            }
        }
    }
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
    use super::*;
    use crate::update::test_support::{executable_stub, test_directory};
    use std::fs;

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
    fn version_mismatch_after_replacement_restores_the_previous_executable() {
        let directory = test_directory();
        let target = directory.join("app.exe");
        let backup = directory.join("app.exe.backup");
        fs::write(&target, executable_stub("1.0.2")).unwrap();
        fs::write(&backup, executable_stub("1.0.1")).unwrap();

        let error = verify_installed_version_or_restore(&target, &backup, "1.0.3").unwrap_err();

        assert!(error.contains("restaurada") || error.contains("restaurado"));
        assert_eq!(read_embedded_build_version(&target).unwrap(), "1.0.1");
        assert!(!backup.exists());
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
    fn helper_executable_path_contains_process_id() {
        let path = helper_executable_path(12345);
        let filename = path.file_name().unwrap().to_string_lossy();
        assert_eq!(filename, "P2P-Voz-e-tela-update-helper-12345.exe");
    }

    #[test]
    fn stage_and_launch_fails_for_non_existent_file() {
        let missing = PathBuf::from("non-existent-update.exe");
        let result = stage_and_launch(missing, "1.0.0".to_owned());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("não foi encontrado"));
    }

    #[test]
    fn stage_and_launch_rejects_version_mismatch() {
        let directory = test_directory();
        let file = directory.join("downloaded.exe");
        fs::write(&file, executable_stub("1.0.1")).unwrap();
        let result = stage_and_launch(file, "1.0.2".to_owned());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("esperava a versao 1.0.2"));
        let _ = fs::remove_dir_all(directory);
    }
}
