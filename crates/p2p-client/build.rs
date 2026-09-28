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
    let resource_compiler = env::var_os("RC").unwrap_or_else(|| "rc.exe".into());

    let status = Command::new(resource_compiler)
        .current_dir(&assets_dir)
        .arg("/nologo")
        .arg(format!("/fo{}", resource_file.display()))
        .arg("p2p-client.rc")
        .status()
        .expect("não foi possível iniciar o compilador de recursos do Windows (rc.exe)");

    assert!(
        status.success(),
        "rc.exe falhou ao compilar o ícone do aplicativo"
    );
    println!("cargo:rustc-link-arg={}", resource_file.display());
}
