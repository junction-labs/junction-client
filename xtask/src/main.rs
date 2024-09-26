use std::env;

use clap::{Parser, Subcommand};
use xshell::{cmd, Shell};

fn main() -> anyhow::Result<()> {
    let sh = Shell::new()?;
    let args = Args::parse();

    let venv = env::var("VIRTUAL_ENV").unwrap_or_else(|_| ".venv".to_string());

    match &args.command {
        Commands::PythonClean => python::clean(&sh, &venv),
        Commands::PythonBuild {
            skip_maturin,
            skip_stubs,
        } => python::build(&sh, &venv, !*skip_maturin, !*skip_stubs),
        Commands::PythonLint { fix } => python::lint(&sh, &venv, *fix),
        Commands::PythonTest => python::test(&sh, &venv),
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
    PythonBuild {
        /// Skip rebuilding the junction-python wheel. Useful for working on
        /// generating config type information.
        #[clap(long)]
        skip_maturin: bool,

        /// Skip regenerating API stubs. Useful if you're not changing Junction
        /// API types and want to skip calls to `ruff`.
        #[clap(long)]
        skip_stubs: bool,
    },

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

mod python {
    use super::*;

    pub(super) fn clean(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        cmd!(sh, "rm -rf .ruff_cache/").run()?;
        cmd!(sh, "rm -rf .pytest_cache/").run()?;
        cmd!(sh, "rm -rf {venv}").run()?;

        Ok(())
    }

    pub(super) fn build(sh: &Shell, venv: &str, maturin: bool, stubs: bool) -> anyhow::Result<()> {
        ensure_venv(sh, venv)?;

        if maturin {
            maturin_build(sh, venv)?;
        }
        if stubs {
            generate_typing_hints(sh, venv)?;
        }

        Ok(())
    }

    fn maturin_build(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        cmd!(
        sh,
        "{venv}/bin/maturin develop -m junction-python/Cargo.toml --extras=test --features extension-module"
    )
    .run()?;

        Ok(())
    }

    fn generate_typing_hints(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        let generated = cmd!(sh, "cargo run -p junction-api-gen").read()?;
        let config_typing = "junction-python/junction/config.py";
        sh.write_file(config_typing, generated)?;

        cmd!(
            sh,
            "{venv}/bin/ruff check --config junction-python/pyproject.toml --fix {config_typing}"
        )
        .run()?;
        cmd!(
            sh,
            "{venv}/bin/ruff format --config junction-python/pyproject.toml {config_typing}"
        )
        .run()?;

        Ok(())
    }

    pub(super) fn test(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        ensure_venv(sh, venv)?;
        build(sh, venv, true, true)?;

        cmd!(sh, "{venv}/bin/pytest").run()?;

        Ok(())
    }

    pub(super) fn lint(sh: &Shell, venv: &str, fix: bool) -> anyhow::Result<()> {
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
            mk_venv(sh, venv)?;
            install_packages(sh, venv)?;
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
}
