use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;
use xshell::{cmd, Shell};

fn main() -> anyhow::Result<()> {
    // set the current dir BEFORE opening a new shell so that both the
    // shell and things like std::fs::read_to_string act as if they're
    // operating from the repo root.
    let project_root = project_root();
    std::env::set_current_dir(&project_root)?;

    let sh = Shell::new()?;
    let args = Args::parse();
    match &args.command {
        Commands::InstallPrecommit => install_precommit(&sh),
        Commands::Precommit => precommit(&sh, ".venv"),
        Commands::Version { package, json } => version(&sh, *json, package.as_deref()),
        Commands::CheckDiffs => check_diffs(&sh),
        Commands::Core(args) => core::run(&sh, args),
        Commands::Node(args) => node::run(&sh, args),
        Commands::Python(args) => python::run(&sh, args),
    }
}

fn project_root() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .unwrap()
        .to_path_buf()
}

#[derive(Debug, Serialize)]
struct CrateInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    version: semver::Version,
    maj_min_version: String,
    sha: String,
}

fn crate_info<P: AsRef<Path>>(sh: &Shell, path: P) -> anyhow::Result<CrateInfo> {
    fn crate_name(manifest: &toml::Table) -> anyhow::Result<Option<String>> {
        match toml_lookup(manifest, &["package", "name"]) {
            Some(toml::Value::String(name)) => Ok(Some(name.clone())),
            Some(_) => anyhow::bail!("invalid Cargo manifest: package.name is not a string"),
            None => Ok(None),
        }
    }

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

    let name = crate_name(&manifest)?;
    let version = package_version(&manifest).or_else(|_| workspace_version(&manifest))?;
    let maj_min_version = format!("{}.{}", version.major, version.minor);

    let mut sha = cmd!(sh, "git rev-parse HEAD").read()?.trim().to_string();
    if check_diffs(sh).is_err() {
        sha.push_str(" (dirty)");
    }

    Ok(CrateInfo {
        name,
        version,
        maj_min_version,
        sha,
    })
}

/// Cargo xtasks for development.
///
/// Three build systems in a trenchcoat, and your one stop shop for Junction
/// dev. See xtask help <command> for a detailed description of how we build
/// individual client libraries.
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Install `xtask precommit` as a precommit hooks in the local git repo.
    InstallPrecommit,

    /// Run xtask precommit hooks.
    ///
    /// This currently includes linting Rust, Python, and Node libraries on
    /// every commit. Heavier tasks like full builds and tests will only run in
    /// CI.
    Precommit,

    /// Print the versions of all Junction client crates.
    ///
    /// Lists all versions of Junction crates. Core crates are versioned
    /// together in lockstep using the repo root's Cargo.toml, and individual
    /// client libraries are versioned independently. The Cargo.toml in each
    /// client should be treated as the authoritative version number - ecosystem
    /// tools and xtasks should use that version number where possible.
    Version {
        #[clap(short, long)]
        package: Option<String>,

        #[clap(short, long, default_value_t = false)]
        json: bool,
    },

    /// Check the repo to see if there are any uncomitted changes.
    ///
    /// Uses git status to check if anything has been modified or added to the
    /// repo. Fails if any changes have been made.
    CheckDiffs,

    /// junction-core tasks. See xtask help junction-core for details.
    Core(core::Args),

    /// junction-node tasks.
    ///
    /// junction-node is built with neon-rs and npm. To keep things looking like
    /// a simple Neon project, junction-node/package.json contains the steps for
    /// building and packaging the Node library. The end result of that is that
    /// node xtasks tend to call npm, which often calls right back into Cargo.
    /// This is slightly confusing, but ultimately fine.
    Node(node::Args),

    /// junction-python tasks. See xtask help junction-python for details.
    ///
    /// junction-python is built as a standard PyO3 project, and relies on
    /// maturin to build and package wheels.
    ///
    /// python xtasks manage up a virtualenv named .venv at the root of this
    /// repo and use it to build and manage python dependencies. You should
    /// never have to create or activate your own virtualenv. If you want to use
    /// a completely separate env, python xtasks respect the VIRTUAL_ENV
    /// environment variable and treat it as a path to a virtualenv.
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
    node::lint(sh, &["--prefix", "junction-node"], false)?;

    Ok(())
}

fn version(sh: &Shell, json: bool, only: Option<&str>) -> anyhow::Result<()> {
    match only {
        Some(prefix) => {
            let info = crate_info(sh, format!("{prefix}/Cargo.toml"))?;

            if json {
                println!("{}", serde_json::to_string_pretty(&info).unwrap());
            } else {
                println!(
                    "{}: {}",
                    info.name.unwrap_or("workspace".to_string()),
                    info.version
                );
            }
        }
        None => {
            let info = [
                crate_info(sh, "Cargo.toml")?,
                crate_info(sh, "junction-python/Cargo.toml")?,
                crate_info(sh, "junction-node/Cargo.toml")?,
            ];

            if json {
                println!("{}", serde_json::to_string_pretty(&info).unwrap());
            } else {
                for info in info {
                    println!(
                        "{name:>20}: {version}",
                        name = info.name.unwrap_or("workspace".to_string()),
                        version = info.version
                    );
                }
            }
        }
    }

    Ok(())
}

fn check_diffs(sh: &Shell) -> anyhow::Result<()> {
    let changed = cmd!(sh, "git status --porcelain").read()?;
    if !changed.is_empty() {
        anyhow::bail!("found uncomitted changes: \n\n{changed}");
    }

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
        "+env: {k}={v}",
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

        /// Clean the current virtualenv and any caches.
        Clean,

        /// Build docs with sphinx.
        Docs,

        /// Lint and format junction-python.
        Lint {
            /// Try to automatically fix any linter/formatter errors.
            #[clap(long)]
            fix: bool,
        },

        /// Run a `python` shell.
        ///
        /// Builds a fresh version of Junction and installs it before running
        /// python.
        #[cfg(unix)]
        Shell,

        /// Test junction-python.
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
            #[cfg(unix)]
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

    #[cfg(unix)]
    pub(super) fn shell(sh: &Shell, venv: &str) -> anyhow::Result<()> {
        use std::os::unix::process::CommandExt;

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

        cmd!(sh, "cargo test -p junction-python").run()?;

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
            /// Build mode. dev builds a normal cargo build. release uses cargo
            /// build --release, and cross cross-compiles a release build with
            /// cargo-cross, using CARGO_BUILD_TARGET and NEON_BUILD_PLATFORM.
            #[clap(long, default_value_t, value_enum)]
            mode: BuildMode,

            /// Run install with `npm ci` intead of `npm i`.
            #[clap(long)]
            clean_install: bool,
        },

        /// Run npm pack to create tarball ready for uploading to npm.
        Pack {
            /// Build the tarball for the given platform. If not specified,
            /// builds the top level cross-platform package.
            #[clap(long)]
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
        #[cfg(unix)]
        Shell,

        /// Update npm package versions from the Cargo manifest.
        Version,
    }

    type NpmArgs<'a> = &'a [&'static str];

    pub(super) fn run(sh: &Shell, args: &Args) -> anyhow::Result<()> {
        let npm_args = ["--prefix", "./junction-node"];

        match &args.command {
            Commands::Build {
                mode,
                clean_install,
            } => build(sh, &npm_args, *clean_install, *mode),
            Commands::Pack { platform } => pack(sh, &npm_args, platform.as_deref()),
            Commands::Clean => clean(sh),
            Commands::Lint { fix } => lint(sh, &npm_args, *fix),
            Commands::Docs => docs(sh, &npm_args),
            Commands::Version => version(sh, &npm_args),
            #[cfg(unix)]
            Commands::Shell => shell(sh, &npm_args),
        }
    }

    pub(super) fn clean(sh: &Shell) -> anyhow::Result<()> {
        cmd!(sh, "rm -rf junction-node/node_modules/").run()?;
        cmd!(sh, "rm -rf junction-node/index.node").run()?;
        cmd!(sh, "rm -rf junction-node/lib/").run()?;
        cmd!(sh, "rm -rf junction-node/platforms/**/index.node").run()?;

        Ok(())
    }

    #[derive(ValueEnum, Clone, Copy, Default)]
    pub(crate) enum BuildMode {
        #[default]
        Dev,
        Release,
        Cross,
    }

    fn build(
        sh: &Shell,
        npm_args: NpmArgs,
        clean_install: bool,
        mode: BuildMode,
    ) -> anyhow::Result<()> {
        let _env = loud_env(sh, "JUNCTION_CLIENT_SKIP_POSTINSTALL", "true");

        let install_cmd = if clean_install { "ci" } else { "i" };
        cmd!(sh, "npm {npm_args...} {install_cmd} --fund=false").run()?;

        let build_cmd = match mode {
            BuildMode::Dev => "build",
            BuildMode::Release => "build-release",
            BuildMode::Cross => "cross-release",
        };

        let _build_env = if matches!(mode, BuildMode::Cross) {
            let build_target = env::var("XTASK_BUILD_TARGET").map_err(|_| {
                anyhow::anyhow!("can't cross compile without XTASK_BUILD_TARGET set")
            })?;
            Some(loud_env(sh, "CARGO_BUILD_TARGET", build_target))
        } else {
            None
        };

        cmd!(sh, "npm {npm_args...} run {build_cmd}").run()?;

        Ok(())
    }

    fn docs(sh: &Shell, npm_args: NpmArgs) -> anyhow::Result<()> {
        build(sh, npm_args, true, BuildMode::Dev)?;

        cmd!(sh, "npm {npm_args...} run doc").run()?;

        Ok(())
    }

    fn pack(sh: &Shell, npm_args: NpmArgs, platform: Option<&str>) -> anyhow::Result<()> {
        let dest_dir = "junction-node/dist";
        let package = match platform {
            Some(platform) => format!("./junction-node/platforms/{platform}"),
            None => "./junction-node".to_string(),
        };

        cmd!(sh, "mkdir -p {dest_dir}").run()?;
        cmd!(
            sh,
            "npm {npm_args...} pack {package} --pack-destination {dest_dir}"
        )
        .run()?;

        Ok(())
    }

    pub(super) fn lint(sh: &Shell, npm_args: NpmArgs, fix: bool) -> anyhow::Result<()> {
        // rustfmt
        rustfmt_check(sh, "junction-node")?;

        // run npm lint/fix
        let lint_cmd = if fix { "fix" } else { "lint" };
        cmd!(sh, "npm {npm_args...} run {lint_cmd}").run()?;

        Ok(())
    }

    #[cfg(unix)]
    fn shell(sh: &Shell, npm_args: NpmArgs) -> anyhow::Result<()> {
        use std::os::unix::process::CommandExt;

        build(sh, npm_args, true, BuildMode::Dev)?;

        // change dir and exec. we're not coming back to xtasks, we're either
        // going to exec or exit, so don't have to worry about resetting the dir
        // when we're done.
        sh.change_dir("junction-node");
        let mut cmd: std::process::Command = cmd!(sh, "node").into();
        Err(cmd.exec().into())
    }

    fn version(sh: &Shell, npm_args: NpmArgs) -> anyhow::Result<()> {
        let crate_info = crate_info(sh, "junction-node/Cargo.toml")?;
        let platform_package_dirs = cmd!(sh, "ls junction-node/platforms/").read()?;

        let platforms: Vec<_> = platform_package_dirs
            .split_whitespace()
            .map(|s| s.trim().to_string())
            .collect();

        update_ts_package(
            "junction-node/package.json",
            &crate_info.version,
            &platforms,
        )?;
        for platform in &platforms {
            let pkg_path = format!("junction-node/platforms/{platform}/package.json");
            update_platform_package(pkg_path, &crate_info.version)?;
        }

        // run lint to clean up again
        build(sh, npm_args, false, BuildMode::Dev)?;
        lint(sh, npm_args, true)?;

        Ok(())
    }

    fn update_ts_package<P: AsRef<Path>>(
        path: P,
        version: &semver::Version,
        platforms: &[String],
    ) -> anyhow::Result<()> {
        let content = std::fs::read_to_string(&path)?;
        let serde_json::Value::Object(mut package) = serde_json::from_str(&content)? else {
            anyhow::bail!(
                "{path} was not a json object",
                path = path.as_ref().display()
            );
        };

        let platform_versions: serde_json::Map<_, _> = platforms
            .iter()
            .map(|platform| {
                let platform_pkg = format!("@junction-labs/client-{platform}");
                let pkg_version = serde_json::Value::String(version.to_string());
                (platform_pkg, pkg_version)
            })
            .collect();

        package["version"] = serde_json::Value::String(version.to_string());
        package["optionalDependencies"] = serde_json::Value::Object(platform_versions);

        let output = serde_json::to_string_pretty(&package)?;
        std::fs::write(path, output)?;

        Ok(())
    }

    fn update_platform_package<P: AsRef<Path>>(
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
