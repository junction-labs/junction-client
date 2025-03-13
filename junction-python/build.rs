use std::process::Command;

fn main() {
    // pyo3 requires special linker args on macos. instead of setting .cargo/config.toml
    // we can do that here instead:
    //
    // https://pyo3.rs/v0.22.2/building-and-distribution#macos
    pyo3_build_config::add_extension_module_link_args();

    // set BUILD_SHA as an env var
    let short_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .expect("failed to get build version");
    let short_sha = std::str::from_utf8(short_sha.stdout.trim_ascii()).unwrap();

    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .expect("failed to get git status");
    let dirty = if status.stdout.trim_ascii().is_empty() {
        ""
    } else {
        "-dirty"
    };
    println!("cargo::rustc-env=BUILD_SHA={short_sha}{dirty}")
}
