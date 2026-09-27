fn main() {
    println!("cargo:rerun-if-env-changed=P2P_UPDATE_MANIFEST_URL");
    let manifest_url = std::env::var("P2P_UPDATE_MANIFEST_URL").unwrap_or_default();
    println!("cargo:rustc-env=P2P_UPDATE_MANIFEST_URL={manifest_url}");
}
