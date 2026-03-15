fn main() {
    // Link system libfdk-aac (install libfdk-aac-dev on Debian/Ubuntu).
    println!("cargo:rustc-link-lib=fdk-aac");
}
