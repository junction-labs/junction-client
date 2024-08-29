use std::env;

use clap::{Parser, Subcommand};
use xshell::{cmd, Shell};

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let sh = Shell::new()?;

    let venv = env::var("VENV").unwrap_or_else(|_| ".venv".to_string());

    match &args.command {
        Commands::PythonClean => clean(&sh, &venv),
        Commands::PythonBuild => python_build(&sh, &venv),
        Commands::PythonTest => python_test(&sh, &venv),
    }
}

/// Cargo `xtasks` for development.
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build and install junction-python in a .venv.
    PythonBuild,

    /// Run all junction-python tests, both in Rust and in Python.
    PythonTest,

    /// Clean the current virtualenv and any Python caches.
    PythonClean,
}

fn clean(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    cmd!(sh, "rm -rf .ruff_cache/").run()?;
    cmd!(sh, "rm -rf {venv}").run()?;

    Ok(())
}

fn setup_venv(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    mk_venv(sh, venv)?;
    install_packages(sh, venv)?;

    Ok(())
}

fn python_build(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    ensure_venv(sh, venv)?;

    cmd!(
        sh,
        "{venv}/bin/maturin develop -m junction-python/Cargo.toml --extras=test"
    )
    .run()?;

    Ok(())
}

fn python_test(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    ensure_venv(sh, venv)?;
    python_build(sh, venv)?;

    cmd!(sh, "cargo test -p junction-python").run()?;
    cmd!(sh, "{venv}/bin/pytest").run()?;

    Ok(())
}

fn ensure_venv(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    if !std::fs::metadata(venv).is_ok_and(|m| m.is_dir()) {
        setup_venv(sh, venv)?;
    }

    Ok(())
}

fn mk_venv(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    cmd!(sh, "python3 -m venv {venv}").run()?;

    Ok(())
}

fn install_packages(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    cmd!(sh, "{venv}/bin/python -m pip install --upgrade uv").run()?;
    cmd!(
        sh,
        "{venv}/bin/uv pip install --upgrade --compile-bytecode -r junction-python/requirements-dev.txt"
    )
    .run()?;

    Ok(())
}
