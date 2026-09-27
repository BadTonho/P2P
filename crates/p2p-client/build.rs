fn main() {
    println!("cargo:rerun-if-env-changed=P2P_UPDATE_DRIVE_FOLDER_ID");
    println!("cargo:rerun-if-env-changed=P2P_UPDATE_DRIVE_API_KEY");

    let folder_id = std::env::var("P2P_UPDATE_DRIVE_FOLDER_ID")
        .unwrap_or_else(|_| "1byA2jwpqhjICOeZWeI1i_Lfn2KDqr4fh".to_owned());
    let api_key = std::env::var("P2P_UPDATE_DRIVE_API_KEY").unwrap_or_default();

    println!("cargo:rustc-env=P2P_UPDATE_DRIVE_FOLDER_ID={folder_id}");
    println!("cargo:rustc-env=P2P_UPDATE_DRIVE_API_KEY={api_key}");
}
