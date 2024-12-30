use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand, ValueEnum};
use xshell::{cmd, Shell};

fn main() -> anyhow::Result<()> {
    let sh = Shell::new()?;
    sh.change_dir(project_root());

    let args = Args::parse();

    let venv = env::var("VIRTUAL_ENV").unwrap_or_else(|_| ".venv".to_string());

    use Commands::*;
    match &args.command {
        InstallPrecommit => install_precommit(&sh),
        Precommit => precommit(&sh, &venv),
        // rust
        CITest => rust::ci_test(&sh),
        CIDoc => rust::ci_doc(&sh),
        CIClippy {
            crates,
            fix,
            allow_staged,
        } => rust::ci_clippy(&sh, crates, *fix, *allow_staged),
        // node
        NodeBuild { clean_install } => node::build(&sh, *clean_install),
        NodeClean => node::clean(&sh),
        NodeLint { fix } => node::lint(&sh, *fix),
        NodeShell => node::shell(&sh),
        // python
        PythonBuild {
            maturin,
            skip_stubs,
        } => python::build(&sh, &venv, maturin, !*skip_stubs),
        PythonClean => python::clean(&sh, &venv),
        PythonDocs => python::docs(&sh, &venv),
        PythonLint { fix } => python::lint(&sh, &venv, *fix),
        PythonTest => python::test(&sh, &venv),
        PythonShell => python::shell(&sh, &venv),
    }
}

fn project_root() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
}

/// Cargo xtasks for development.
///
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
    /// Install xtask precommit hooks in the local git repo.
    InstallPrecommit,

    /// Run xtask precommit hooks.
    ///
    /// This currently includes linting Rust, Python, and Node libraries on
    /// every commit. Heavier tasks like full builds and tests will only run in
    /// CI.
    Precommit,

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

    /// Run tests for all core crates with appropriate features enabled.
    CITest,

    /// Run `cargo doc` for junction-core and junction-api with the appropriate
    /// features set for public docs.
    CIDoc,

    /// Build the Node native extension and compile typescript.
    ///
    /// Does not build a release build. To build a release build in CI, use
    /// Cargo and npm directly.
    NodeBuild {
        /// Run install with `npm ci` intead of `npm i`.
        #[clap(long)]
        clean_install: bool,
    },

    /// Clean up the current node_modules and remove any built native
    /// extensions.
    NodeClean,

    /// Lint and format Typescript and Javascript.
    NodeLint {
        /// Try to automatically fix any linter/formatter errors.
        #[clap(long)]
        fix: bool,
    },

    /// Run a `node` repl. Builds a fresh debug version of Junction Node before
    /// starting the shell.
    NodeShell,

    /// Build and install junction-python in a .venv.
    ///
    /// Does not build a release build. In CI, use maturin directly.
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

    /// Clean the current virtualenv and any Python caches.
    PythonClean,

    /// Build Python docs with sphinx.
    PythonDocs,

    /// Lint and format Python code.
    PythonLint {
        /// Try to automatically fix any linter/formatter errors.
        #[clap(long)]
        fix: bool,
    },

    /// Run a `python` repl in the current virtual environment. Builds a fresh
    /// version of Junction Python and installs it before starting.
    PythonShell,

    /// Run junction-python's Python tests.
    PythonTest,
}

fn install_precommit(sh: &Shell) -> anyhow::Result<()> {
    cmd!(sh, "git config --local core.hooksPath xtask/hooks").run()?;

    Ok(())
}

fn precommit(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    // clippy everything
    rust::ci_clippy::<&'static str>(sh, &[], false, false)?;
    rust::ci_clippy(sh, &["junction-python", "junction-node"], false, false)?;

    // regenerate SDKs and lint. verify that they're not going to cause a diff.
    python::generate(sh, venv)?;

    // per-language lints
    python::lint(sh, venv, false)?;
    node::lint(sh, false)?;

    Ok(())
}

mod rust {
    use super::*;

    pub(super) fn ci_test(sh: &Shell) -> anyhow::Result<()> {
        #[rustfmt::skip]
        let default_features = [
            "-F", "junction-api/kube", "-F", "junction-api/xds",
        ];

        // relies on the fact that Cargo.toml has all crates in crates/* listed
        // as default targets
        cmd!(sh, "cargo test {default_features...}").run()?;

        Ok(())
    }

    pub(super) fn ci_doc(sh: &Shell) -> anyhow::Result<()> {
        let _rustdoc_flags = loud_env(
            sh,
            "RUSTDOCFLAGS",
            "--cfg docsrs -D warnings --allow=rustdoc::redundant-explicit-links",
        );

        #[rustfmt::skip]
        let crate_args = [
            "-p", "junction-api", "-F", "junction-api/kube", "-F", "junction-api/xds",
            "-p", "junction-core",
        ];

        cmd!(sh, "cargo doc --no-deps {crate_args...}").run()?;

        Ok(())
    }

    pub(super) fn ci_clippy<S: AsRef<str>>(
        sh: &Shell,
        crates: &[S],
        fix: bool,
        allow_staged: bool,
    ) -> anyhow::Result<()> {
        let crate_args = crate_args(crates);

        let mut options = vec![
            "--tests",
            "-F",
            "junction-api/xds",
            "-F",
            "junction-api/kube",
            "--no-deps",
        ];
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

    fn crate_args(crates: &[impl AsRef<str>]) -> Vec<&str> {
        crates
            .iter()
            .map(|name| ["-p", name.as_ref()])
            .flatten()
            .collect()
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
    use std::os::unix::process::CommandExt;

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
            generate(sh, venv)?;
        }

        Ok(())
    }

    pub(super) fn shell(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        ensure_venv(sh, venv)?;
        build(sh, venv, &MaturinOption::Develop, true)?;

        let mut cmd: std::process::Command = cmd!(sh, "{venv}/bin/python").into();
        Err(cmd.exec().into())
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

    pub(super) fn generate(sh: &Shell, venv: &str) -> anyhow::Result<()> {
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

    pub(super) fn docs(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        ensure_venv(sh, venv)?;

        cmd!(
            sh,
            "{venv}/bin/uv pip install --upgrade --compile-bytecode -r junction-python/docs/requirements.txt"
        ).run()?;

        let _dir = sh.push_dir("junction-python/docs/");
        cmd!(
            sh,
            "../../.venv/bin/sphinx-build -M html source build -j auto -W"
        )
        .run()?;

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

mod node {
    use std::os::unix::process::CommandExt;

    use super::*;

    pub(super) fn clean(sh: &Shell) -> anyhow::Result<()> {
        cmd!(sh, "rm -rf junction-node/node_modules/").run()?;
        cmd!(sh, "rm -rf junction-node/index.node").run()?;
        cmd!(sh, "rm -rf junction-node/platforms/**/index.node").run()?;

        Ok(())
    }

    pub(super) fn build(sh: &Shell, clean_install: bool) -> anyhow::Result<()> {
        let _dir = sh.push_dir("junction-node");

        let install_cmd = if clean_install { "ci" } else { "i" };
        cmd!(sh, "npm {install_cmd} --fund=false").run()?;
        cmd!(sh, "npm run build").run()?;

        Ok(())
    }

    pub(super) fn lint(sh: &Shell, fix: bool) -> anyhow::Result<()> {
        let _dir = sh.push_dir("junction-node");

        cmd!(sh, "npm install --fund=false").run()?;

        let lint_cmd = if fix { "fix" } else { "lint" };
        cmd!(sh, "npm run {lint_cmd}").run()?;

        Ok(())
    }

    pub(super) fn shell(sh: &Shell) -> anyhow::Result<()> {
        let _dir = sh.push_dir("junction-node");

        cmd!(sh, "npm install --fund=false").run()?;
        cmd!(sh, "npm run build").run()?;

        let mut cmd: std::process::Command = cmd!(sh, "node").into();
        Err(cmd.exec().into())
    }
}
