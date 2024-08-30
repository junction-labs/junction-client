fn main() {
    // pyo3 requires special linker args on macos. instead of setting .cargo/config.toml
    // we can do that here instead:
    //
    // https://pyo3.rs/v0.22.2/building-and-distribution#macos
    pyo3_build_config::add_extension_module_link_args();
}
