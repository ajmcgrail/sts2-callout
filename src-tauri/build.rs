fn main() {
    // whisper-rs-sys decides this by the *host* OS, so cross-compiles miss it;
    // ggml reads CPU info from the registry.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            println!("cargo:rustc-link-arg=advapi32.lib");
        } else {
            println!("cargo:rustc-link-arg=-ladvapi32");
        }
    }
    tauri_build::build()
}
