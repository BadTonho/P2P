use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/p2p-client.rc");
    println!("cargo:rerun-if-changed=assets/p2p-client.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let assets_dir = manifest_dir.join("assets");
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").expect("out dir"));
    let resource_file = output_dir.join("p2p-client.res");
    let resource_compiler = find_resource_compiler();

    let status = Command::new(&resource_compiler)
        .current_dir(&assets_dir)
        .arg("/nologo")
        .arg(format!("/fo{}", resource_file.display()))
        .arg("p2p-client.rc")
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "não foi possível iniciar o compilador de recursos do Windows ({:?}): {error}",
                resource_compiler
            )
        });

    assert!(
        status.success(),
        "rc.exe falhou ao compilar o ícone do aplicativo"
    );
    println!("cargo:rustc-link-arg={}", resource_file.display());
}

fn find_resource_compiler() -> std::ffi::OsString {
    if let Some(rc) = env::var_os("RC") {
        return rc;
    }

    if Command::new("rc.exe").arg("/?").output().is_ok() {
        return "rc.exe".into();
    }

    let search_roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];

    for root in search_roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };

        let mut version_dirs = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                version_dirs.push(path);
            }
        }
        version_dirs.sort();
        version_dirs.reverse();

        for dir in version_dirs {
            let candidate = dir.join("x64").join("rc.exe");
            if candidate.is_file() {
                return candidate.into_os_string();
            }
        }
    }

    "rc.exe".into()
}
