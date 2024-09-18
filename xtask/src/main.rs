use std::env;

use clap::{Parser, Subcommand};
use xshell::{cmd, Shell};

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let sh = Shell::new()?;

    let venv = env::var("VIRTUAL_ENV").unwrap_or_else(|_| ".venv".to_string());

    match &args.command {
        Commands::PythonClean => clean(&sh, &venv),
        Commands::PythonBuild => python_build(&sh, &venv),
        Commands::PythonLint { fix } => python_lint(&sh, &venv, *fix),
        Commands::PythonTest => python_test(&sh, &venv),
    }
}

/// Cargo `xtasks` for development.
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[allow(clippy::enum_variant_names)]
#[derive(Subcommand)]
enum Commands {
    /// Build and install junction-python in a .venv.
    PythonBuild,

    /// Run junction-python's Python tests.
    PythonTest,

    /// Lint and format Python code.
    PythonLint {
        /// Try to automatically fix any linter/formatter errors.
        #[clap(long)]
        fix: bool,
    },

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
        "{venv}/bin/maturin develop -m junction-python/Cargo.toml --extras=test --features extension-module"
    )
    .run()?;

    Ok(())
}

fn python_test(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    ensure_venv(sh, venv)?;
    python_build(sh, venv)?;

    cmd!(sh, "{venv}/bin/pytest").run()?;

    Ok(())
}

fn python_lint(sh: &Shell, venv: &str, fix: bool) -> anyhow::Result<()> {
    ensure_venv(sh, venv)?;

    if !fix {
        // when not fixing, always run both checks and return an error if either
        // fails. it's annoying to not see all the errors at first.
        let check = cmd!(
            sh,
            "{venv}/bin/ruff check --config junction-python/pyproject.toml --no-fix"
        )
        .run();
        let format = cmd!(
            sh,
            "{venv}/bin/ruff format --config junction-python/pyproject.toml --check"
        )
        .run();
        check.and(format)?;
    } else {
        // when fixing, run sequentially in case there's a change in formatting
        // that a fix would introduce (that would be annoying buuuuuuut).
        cmd!(
            sh,
            "{venv}/bin/ruff check --config junction-python/pyproject.toml --fix"
        )
        .run()?;
        cmd!(
            sh,
            "{venv}/bin/ruff format --config junction-python/pyproject.toml"
        )
        .run()?;
    }

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
