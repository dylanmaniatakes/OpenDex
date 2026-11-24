fn main() {
    if std::env::var_os("CARGO_FEATURE_DESKTOP").is_some() {
        println!("cargo:rerun-if-changed=ui");
        println!("cargo:rerun-if-changed=icons/icon.png");
        tauri_build::build();
    }
}
