fn main() {
    // Link system libfdk-aac only when the "fdk-aac" feature is enabled.
    // Install libfdk-aac-dev on Debian/Ubuntu.
    if std::env::var("CARGO_FEATURE_FDK_AAC").is_ok() {
        println!("cargo:rustc-link-lib=fdk-aac");
    }
}
