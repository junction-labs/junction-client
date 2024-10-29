use std::{env, ffi::OsStr};

use clap::{Parser, Subcommand, ValueEnum};
use xshell::{cmd, Shell};

fn main() -> anyhow::Result<()> {
    let sh = Shell::new()?;
    let args = Args::parse();

    let venv = env::var("VIRTUAL_ENV").unwrap_or_else(|_| ".venv".to_string());

    match &args.command {
        // rust
        Commands::Check { crates } => rust::cargo_cmd(&sh, "check", &[], crates),
        Commands::Doc { crates, deps, ci } => rust::docs(&sh, crates, *deps, *ci),
        Commands::CIClippy {
            crates,
            fix,
            allow_staged,
        } => rust::ci_clippy(&sh, crates, *fix, *allow_staged),
        // python
        Commands::PythonClean => python::clean(&sh, &venv),
        Commands::PythonBuild {
            maturin,
            skip_stubs,
        } => python::build(&sh, &venv, maturin, !*skip_stubs),
        Commands::PythonLint { fix } => python::lint(&sh, &venv, *fix),
        Commands::PythonTest => python::test(&sh, &venv),
    }
}

/// Cargo xtasks for development.
///
/// xtasks are here anything common that takes more than a single, standard
/// cargo command. If a cargo default isn't working, try running `cargo xtask
/// --help` to see if there's an equivalent here.
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(ValueEnum, Clone, Default)]
enum MaturinOption {
    None,
    #[default]
    Develop,
    Build,
}

#[allow(clippy::enum_variant_names)]
#[derive(Subcommand)]
enum Commands {
    /// Run `cargo check` for a crate or crates, with appropriate environment
    /// variables set.
    Check {
        /// The crates to check.
        #[clap(long, num_args=1.., default_value = "junction-api")]
        crates: Vec<String>,
    },

    /// Run `cargo clippy` with some extra lints, and deny all default warnings.
    CIClippy {
        /// The crates to check. Defaults to the workspace defaults.
        #[clap(long, num_args=0..)]
        crates: Vec<String>,

        /// Automatically fix any errors when possible. See `cargo clippy --fix`
        /// for more detail.
        #[clap(long)]
        fix: bool,

        /// Allows `ci-clippy --fix` to make changes even if there are staged
        /// changes in the current repo.
        #[clap(long)]
        allow_staged: bool,
    },

    /// Run `cargo doc` for a crate or crates, appropriate environment
    /// variables set.
    Doc {
        /// The crates to run rustdoc for.
        #[clap(long, num_args=1.., default_value = "junction-api")]
        crates: Vec<String>,

        /// Run docs for all deps. By default, `doc` runs `cargo doc --no-deps`.
        #[clap(long)]
        deps: bool,

        /// When set, run as a CI check to make sure that docs are valid and
        /// will render correctly on docsrs.
        #[clap(long)]
        ci: bool,
    },

    /// Build and install junction-python in a .venv.
    PythonBuild {
        /// Skip rebuilding the junction-python wheel. Useful for working on
        /// generating config type information.
        #[clap(long, default_value_t, value_enum)]
        maturin: MaturinOption,

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

mod rust {
    use super::*;

    const K8S_OPENAPI_VERSION: &'static str = "K8S_OPENAPI_ENABLED_VERSION";

    pub(super) fn cargo_cmd(
        sh: &Shell,
        cmd: &'static str,
        args: &[&'static str],
        crates: &[String],
    ) -> anyhow::Result<()> {
        let crate_args: Vec<_> = crates.iter().map(|name| ["-p", &name]).flatten().collect();

        let _env = loud_env(sh, K8S_OPENAPI_VERSION, "1.29");
        cmd!(sh, "cargo {cmd} {args...} {crate_args...}").run()?;

        Ok(())
    }

    pub(super) fn docs(sh: &Shell, crates: &[String], deps: bool, ci: bool) -> anyhow::Result<()> {
        let crate_args = crate_args(crates);

        let _k8s_version = loud_env(sh, K8S_OPENAPI_VERSION, "1.29");
        let _rustdoc_flags = if ci {
            Some(loud_env(
                sh,
                "RUSTDOCFLAGS",
                "--cfg docsrs -D warnings --allow=rustdoc::redundant-explicit-links",
            ))
        } else {
            None
        };

        let args = if deps { vec![] } else { vec!["--no-deps"] };

        cmd!(sh, "cargo doc {args...} {crate_args...}").run()?;

        Ok(())
    }

    pub(super) fn ci_clippy(
        sh: &Shell,
        crates: &[String],
        fix: bool,
        allow_staged: bool,
    ) -> anyhow::Result<()> {
        let crate_args = crate_args(crates);

        let mut options = vec!["--tests", "--all-features", "--no-deps"];
        if fix {
            options.push("--fix");
        }
        if allow_staged {
            options.push("--allow-staged");
        }

        #[rustfmt::skip]
        let args = vec![
            "-D", "warnings",
            "-D", "clippy::dbg_macro",
        ];

        cmd!(sh, "cargo clippy {crate_args...} {options...} -- {args...}").run()?;

        Ok(())
    }

    fn crate_args(crates: &[String]) -> Vec<&str> {
        crates.iter().map(|name| ["-p", &name]).flatten().collect()
    }
}

fn loud_env<K: AsRef<OsStr>, V: AsRef<OsStr>>(sh: &Shell, key: K, value: V) -> xshell::PushEnv<'_> {
    eprintln!(
        "env: {k}={v}",
        k = key.as_ref().to_string_lossy(),
        v = value.as_ref().to_string_lossy()
    );
    sh.push_env(key, value)
}

mod python {
    use super::*;

    pub(super) fn clean(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        cmd!(sh, "rm -rf .ruff_cache/").run()?;
        cmd!(sh, "rm -rf .pytest_cache/").run()?;
        cmd!(sh, "rm -rf {venv}").run()?;

        Ok(())
    }

    pub(super) fn build(
        sh: &Shell,
        venv: &str,
        maturin: &MaturinOption,
        stubs: bool,
    ) -> anyhow::Result<()> {
        ensure_venv(sh, venv)?;

        match maturin {
            MaturinOption::Develop => maturin_develop(sh, venv)?,
            MaturinOption::Build => maturin_build(sh, venv)?,
            MaturinOption::None => (),
        }

        if stubs {
            generate_typing_hints(sh, venv)?;
        }

        Ok(())
    }

    fn maturin_develop(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        cmd!(
            sh,
            "{venv}/bin/maturin develop -m junction-python/Cargo.toml --extras=test"
        )
        .run()?;

        Ok(())
    }

    fn maturin_build(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        cmd!(sh, "{venv}/bin/maturin build -m junction-python/Cargo.toml").run()?;

        Ok(())
    }

    fn generate_typing_hints(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        let generate_cmd = cmd!(sh, "cargo run -p junction-api-gen");
        // .run() doesn't echo the command. do it ourselves
        //
        // https://github.com/matklad/xshell/issues/57
        eprintln!("$ {}", generate_cmd);

        let generated = generate_cmd.read()?;
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
        build(sh, venv, &MaturinOption::Develop, true)?;

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
