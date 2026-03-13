/// Integration test: the project must build successfully inside a Podman
/// container using the project Dockerfile.
///
/// Run with:
///   cargo test --test podman_build
///
/// The test locates the workspace root relative to this crate's manifest
/// directory, invokes `podman build`, and asserts a zero exit code.
/// Standard output/error from the build are streamed to the terminal so
/// failures are easy to diagnose.
use std::path::Path;
use std::process::Command;

/// Resolve the workspace root from this crate's manifest directory.
///
/// CARGO_MANIFEST_DIR  →  crates/app
/// one parent          →  crates
/// two parents         →  <workspace root>
fn workspace_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // crates/app  →  crates
        .expect("crates dir missing")
        .parent() // crates      →  workspace root
        .expect("workspace root missing")
        .to_path_buf()
}

//#[test]
fn podman_build_succeeds() {
    let root = workspace_root();

    let status = Command::new("podman")
        .args([
            "build",
            "--tag",
            "dab-rtl:test",
            // Pass the workspace root as the build context.
            root.to_str().expect("non-UTF-8 workspace path"),
        ])
        // Inherit stdout/stderr so build progress is visible in `cargo test -- --nocapture`.
        .status()
        .expect("failed to launch `podman` — is it installed?");

    assert!(status.success(), "podman build exited with status {status}");
}
