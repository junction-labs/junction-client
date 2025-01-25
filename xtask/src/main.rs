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

    use Commands::*;
    match &args.command {
        InstallPrecommit => install_precommit(&sh),
        Precommit => precommit(&sh, ".venv"),
        Version => version(),
        Core(args) => core::run(&sh, args),
        Node(args) => node::run(&sh, args),
        Python(args) => python::run(&sh, args),
    }
}

fn project_root() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
}

fn crate_version<P: AsRef<Path>>(path: P) -> anyhow::Result<semver::Version> {
    fn package_version(manifest: &toml::Table) -> anyhow::Result<semver::Version> {
        let Some(toml::Value::String(version)) = toml_lookup(manifest, &["package", "version"])
        else {
            anyhow::bail!("invalid Cargo manifest: missing package.version");
        };

        let version = version.parse()?;
        Ok(version)
    }

    fn workspace_version(manifest: &toml::Table) -> anyhow::Result<semver::Version> {
        let Some(toml::Value::String(version)) =
            toml_lookup(manifest, &["workspace", "package", "version"])
        else {
            anyhow::bail!("invalid cargo manifest: missing workspace.package.version")
        };

        let version = version.parse()?;
        Ok(version)
    }

    fn toml_lookup<'a>(
        table: &'a toml::Table,
        key_path: &[&'static str],
    ) -> Option<&'a toml::Value> {
        match key_path {
            [] => None,
            [k] => table.get(*k),
            [keys @ .., last] => {
                let mut table = table;
                for key in keys {
                    match table.get(*key) {
                        Some(toml::Value::Table(t)) => table = t,
                        _ => return None,
                    }
                }
                table.get(*last)
            }
        }
    }
    let cargo_toml = std::fs::read_to_string(path)?;
    let manifest: toml::Table = cargo_toml.parse()?;

    package_version(&manifest).or_else(|_| workspace_version(&manifest))
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

    /// Print the versions of Junction client crates.
    Version,

    /// junction-core tasks
    Core(core::Args),

    /// junction-node tasks
    Node(node::Args),

    /// junction-python tasks
    Python(python::Args),
}

fn install_precommit(sh: &Shell) -> anyhow::Result<()> {
    cmd!(sh, "git config --local core.hooksPath xtask/hooks").run()?;

    Ok(())
}

fn precommit(sh: &Shell, venv: &str) -> anyhow::Result<()> {
    // build and clippy junction-core
    core::clippy(sh, false, false)?;
    core::fmt(sh)?;

    // regenerate SDKs and verify that they're not going to cause a diff.
    python::generate(sh, venv)?;

    // run per-language lints
    python::lint(sh, venv, false)?;
    node::lint(sh, false)?;

    Ok(())
}

fn version() -> anyhow::Result<()> {
    let core_version = crate_version("Cargo.toml")?;
    let python_version = crate_version("junction-python/Cargo.toml")?;
    let node_version = crate_version("junction-node/Cargo.toml")?;
    eprintln!("  junction-core: {core_version}");
    eprintln!("junction-python: {python_version}");
    eprintln!("  junction-node: {node_version}");

    Ok(())
}

fn clippy(sh: &Shell, crates: &[&str], fix: bool, allow_staged: bool) -> anyhow::Result<()> {
    let crate_args: Vec<_> = crates.iter().flat_map(|name| ["-p", name]).collect();

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

    let args = vec!["-D", "warnings", "-D", "clippy::dbg_macro"];

    cmd!(sh, "cargo clippy {crate_args...} {options...} -- {args...}").run()?;

    Ok(())
}

fn rustfmt_check(sh: &Shell, path_prefix: &str) -> anyhow::Result<()> {
    let ls_files = cmd!(sh, "git ls-files {path_prefix}/*.rs").read()?;
    let rust_files: Vec<_> = ls_files.split_whitespace().collect();
    cmd!(sh, "cargo fmt --check -- {rust_files...}").run()?;

    Ok(())
}

fn loud_env<K: AsRef<OsStr>, V: AsRef<OsStr>>(sh: &Shell, key: K, value: V) -> xshell::PushEnv<'_> {
    eprintln!(
        "env: {k}={v}",
        k = key.as_ref().to_string_lossy(),
        v = value.as_ref().to_string_lossy()
    );
    sh.push_env(key, value)
}

mod core {
    use super::*;

    #[derive(Parser)]
    pub(super) struct Args {
        #[command(subcommand)]
        pub(super) command: Commands,
    }

    #[derive(Subcommand)]
    pub(super) enum Commands {
        /// Run `cargo clippy` with some extra lints, and deny all default warnings.
        Clippy {
            /// Automatically fix any errors when possible. See `cargo clippy --fix`
            /// for more detail.
            #[clap(long)]
            fix: bool,

            /// Allows `ci-clippy --fix` to make changes even if there are staged
            /// changes in the current repo.
            #[clap(long)]
            allow_staged: bool,
        },

        /// Check that all core code is formatted with rustfmt.
        Fmt,

        /// Run tests for all core crates with appropriate features enabled.
        Test,

        /// Run `cargo doc` for junction-core and junction-api with the appropriate
        /// features set for public docs.
        Doc,
    }

    pub(super) fn run(sh: &Shell, args: &Args) -> anyhow::Result<()> {
        match &args.command {
            Commands::Clippy { fix, allow_staged } => clippy(sh, *fix, *allow_staged),
            Commands::Fmt => fmt(sh),
            Commands::Test => test(sh),
            Commands::Doc => doc(sh),
        }
    }

    pub(super) fn test(sh: &Shell) -> anyhow::Result<()> {
        #[rustfmt::skip]
        let default_features = [
            "-F", "junction-api/kube", "-F", "junction-api/xds",
        ];

        // relies on the fact that Cargo.toml has all crates in crates/* listed
        // as default targets
        cmd!(sh, "cargo test {default_features...}").run()?;

        Ok(())
    }

    pub(super) fn doc(sh: &Shell) -> anyhow::Result<()> {
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

    pub(super) fn clippy(sh: &Shell, fix: bool, allow_staged: bool) -> anyhow::Result<()> {
        super::clippy(sh, &[], fix, allow_staged)
    }

    pub(super) fn fmt(sh: &Shell) -> anyhow::Result<()> {
        rustfmt_check(sh, "crates/")
    }
}

mod python {
    use std::os::unix::process::CommandExt;

    use super::*;

    #[derive(Parser)]
    pub(super) struct Args {
        #[command(subcommand)]
        pub(super) command: Commands,
    }

    #[derive(Subcommand)]
    pub(super) enum Commands {
        /// Build and install junction-python in a .venv.
        ///
        /// Does not build a release build. In CI, use maturin directly.
        Build {
            /// Skip rebuilding the junction-python wheel. Useful for working on
            /// generating config type information.
            #[clap(long, default_value_t, value_enum)]
            maturin: Maturin,

            /// Skip regenerating API stubs. Useful if you're not changing Junction
            /// API types and want to skip calls to `ruff`.
            #[clap(long)]
            skip_stubs: bool,
        },

        /// Clean the current virtualenv and any  caches.
        Clean,

        /// Build  docs with sphinx.
        Docs,

        /// Lint and format  code.
        Lint {
            /// Try to automatically fix any linter/formatter errors.
            #[clap(long)]
            fix: bool,
        },

        /// Run a `python` repl in the current virtual environment. Builds a fresh
        /// version of Junction  and installs it before starting.
        Shell,

        /// Run junction-python's  tests.
        Test,
    }

    #[derive(ValueEnum, Clone, Default)]
    pub(super) enum Maturin {
        #[default]
        Develop,
        Build,
        Skip,
    }

    pub(super) fn run(sh: &Shell, args: &Args) -> anyhow::Result<()> {
        // TODO: venv arg?
        let venv = env::var("VIRTUAL_ENV").unwrap_or_else(|_| ".venv".to_string());

        match &args.command {
            Commands::Build {
                maturin,
                skip_stubs,
            } => python::build(sh, &venv, maturin, !*skip_stubs),
            Commands::Clean => python::clean(sh, &venv),
            Commands::Docs => python::docs(sh, &venv),
            Commands::Lint { fix } => python::lint(sh, &venv, *fix),
            Commands::Test => python::test(sh, &venv),
            Commands::Shell => python::shell(sh, &venv),
        }
    }

    pub(super) fn clean(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        cmd!(sh, "rm -rf .ruff_cache/").run()?;
        cmd!(sh, "rm -rf .pytest_cache/").run()?;
        cmd!(sh, "rm -rf {venv}").run()?;

        Ok(())
    }

    pub(super) fn build(
        sh: &Shell,
        venv: &str,
        maturin: &Maturin,
        stubs: bool,
    ) -> anyhow::Result<()> {
        mk_venv(sh, venv)?;

        match maturin {
            Maturin::Develop => maturin_develop(sh, venv)?,
            Maturin::Build => maturin_build(sh, venv)?,
            Maturin::Skip => (),
        }

        if stubs {
            generate(sh, venv)?;
        }

        Ok(())
    }

    pub(super) fn shell(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        mk_venv(sh, venv)?;
        build(sh, venv, &Maturin::Develop, true)?;

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
        // .read() doesn't echo the command like .run(). do it ourselves
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
        mk_venv(sh, venv)?;

        build(sh, venv, &Maturin::Develop, true)?;
        cmd!(sh, "{venv}/bin/pytest").run()?;

        Ok(())
    }

    pub(super) fn docs(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        mk_venv(sh, venv)?;

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
        mk_venv(sh, venv)?;

        rustfmt_check(sh, "junction-python")?;

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

    fn mk_venv(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        if !std::fs::metadata(venv).is_ok_and(|m| m.is_dir()) {
            cmd!(sh, "python3 -m venv {venv}").run()?;
            cmd!(sh, "{venv}/bin/python -m pip install --upgrade uv").run()?;
            cmd!(sh, "{venv}/bin/uv pip install --upgrade --compile-bytecode -r junction-python/requirements-dev.txt").run()?;
        }

        Ok(())
    }
}

/// Node and Node Accessories
mod node {
    use std::os::unix::process::CommandExt;

    use super::*;

    #[derive(Parser)]
    pub(super) struct Args {
        #[command(subcommand)]
        pub(super) command: Commands,
    }

    #[derive(clap::Subcommand)]
    pub(super) enum Commands {
        /// Build the junction-node client.
        Build {
            /// Build in release mode.
            #[clap(long)]
            release: bool,

            /// Run install with `npm ci` intead of `npm i`.
            #[clap(long)]
            clean_install: bool,
        },

        /// Create a tarball ready for uploading to npm.
        Dist {
            /// Build the tarball for the given platform. If not specified,
            /// build the top level cross-platform package.
            platform: Option<String>,
        },

        /// Clean up the current node_modules and remove any built native
        /// extensions.
        Clean,

        /// Lint and format Typescript and Javascript.
        Lint {
            /// Try to automatically fix any linter/formatter errors.
            #[clap(long)]
            fix: bool,
        },

        /// Build typescript docs.
        Docs,

        /// Run a `node` repl. Builds a fresh debug version of Junction before
        /// starting the shell.
        Shell,

        /// Update npm package versions from the Cargo manifest.
        Version,
    }

    pub(super) fn run(sh: &Shell, args: &Args) -> anyhow::Result<()> {
        match &args.command {
            Commands::Build {
                release,
                clean_install,
            } => build(sh, *clean_install, *release),
            Commands::Dist { platform } => dist(sh, platform.as_deref()),
            Commands::Clean => clean(sh),
            Commands::Lint { fix } => lint(sh, *fix),
            Commands::Docs => docs(sh),
            Commands::Shell => shell(sh),
            Commands::Version => version(sh),
        }
    }

    pub(super) fn clean(sh: &Shell) -> anyhow::Result<()> {
        cmd!(sh, "rm -rf junction-node/node_modules/").run()?;
        cmd!(sh, "rm -rf junction-node/index.node").run()?;
        cmd!(sh, "rm -rf junction-node/lib/").run()?;
        cmd!(sh, "rm -rf junction-node/platforms/**/index.node").run()?;

        Ok(())
    }

    pub(super) fn build(sh: &Shell, clean_install: bool, release: bool) -> anyhow::Result<()> {
        let _dir = sh.push_dir("junction-node");
        let _env = loud_env(sh, "JUNCTION_CLIENT_SKIP_POSTINSTALL", "true");

        let install_cmd = if clean_install { "ci" } else { "i" };
        let build_cmd = if release { "build-release" } else { "build" };
        cmd!(sh, "npm {install_cmd} --fund=false").run()?;
        cmd!(sh, "npm run {build_cmd}").run()?;

        Ok(())
    }

    pub(super) fn docs(sh: &Shell) -> anyhow::Result<()> {
        let _dir = sh.push_dir("junction-node");
        let _env = loud_env(sh, "JUNCTION_CLIENT_SKIP_POSTINSTALL", "true");
        cmd!(sh, "npm install --fund=false").run()?;
        cmd!(sh, "npx typedoc").run()?;

        Ok(())
    }

    pub(super) fn dist(sh: &Shell, platform: Option<&str>) -> anyhow::Result<()> {
        let _dir = sh.push_dir("junction-node");

        let package = match platform {
            Some(platform) => format!("./platforms/{platform}"),
            None => ".".to_string(),
        };

        cmd!(sh, "npm pack {package} --pack-destination ./dist").run()?;

        Ok(())
    }

    pub(super) fn lint(sh: &Shell, fix: bool) -> anyhow::Result<()> {
        let _dir = sh.push_dir("junction-node");
        let _env = loud_env(sh, "JUNCTION_CLIENT_SKIP_POSTINSTALL", "true");

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

    fn version(sh: &Shell) -> anyhow::Result<()> {
        let crate_version = crate_version("junction-node/Cargo.toml")?;
        let platform_package_dirs = cmd!(sh, "ls junction-node/platforms/").read()?;

        let platform_packages: Vec<_> = platform_package_dirs
            .split_whitespace()
            .map(|dir| format!("junction-node/platforms/{dir}/package.json"))
            .collect();

        write_package_version("junction-node/package.json", &crate_version)?;
        for path in &platform_packages {
            write_package_version(path, &crate_version)?;
        }

        // run lint to clean this shit up again
        lint(sh, true)?;

        Ok(())
    }

    fn write_package_version<P: AsRef<Path>>(
        path: P,
        version: &semver::Version,
    ) -> anyhow::Result<()> {
        let content = std::fs::read_to_string(&path)?;
        let serde_json::Value::Object(mut package) = serde_json::from_str(&content)? else {
            anyhow::bail!(
                "{path} was not a json object",
                path = path.as_ref().display()
            );
        };

        package["version"] = serde_json::Value::String(version.to_string());

        let output = serde_json::to_string_pretty(&package)?;
        std::fs::write(path, output)?;

        Ok(())
    }
}
