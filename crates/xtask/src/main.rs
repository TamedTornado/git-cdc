//! Dependency-free developer task runner invoked through Cargo aliases.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const DATABASE_URL: &str = "postgres://git_lfs_delta:git_lfs_delta@127.0.0.1:55433/git_lfs_delta";
const DEVELOPMENT_TOKEN: &str = "git-lfs-delta-local";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let task = arguments.next().unwrap_or_else(|| "help".into());
    let root = workspace_root()?;
    match task.as_str() {
        "dev" => dev(&root),
        "dev-down" => compose(&root, &["down", "--remove-orphans"]),
        "ci" => ci(&root),
        "acceptance" => command(&root, "bash", &["tests/acceptance.sh"], &[]),
        "release-check" => {
            let tag = arguments
                .next()
                .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));
            if arguments.next().is_some() {
                return Err("release-check accepts at most one tag".into());
            }
            release_check(&root, &tag)
        }
        "help" | "--help" | "-h" => {
            println!(
                "cargo dev         start dependencies, migrate, and run the local server\n\
                 cargo dev-down    stop disposable local dependencies\n\
                 cargo ci          run formatting, lint, and workspace tests\n\
                 cargo acceptance  run the full Docker-backed acceptance suite\n\
                 cargo release-check [TAG]\n\
                                    validate release metadata before publishing"
            );
            Ok(())
        }
        other => Err(format!("unknown task {other:?}; run `cargo run -p xtask -- help`").into()),
    }
}

fn release_check(root: &Path, tag: &str) -> Result<(), Box<dyn Error>> {
    let version = env!("CARGO_PKG_VERSION");
    let expected_tag = format!("v{version}");
    if tag != expected_tag {
        return Err(format!(
            "release tag {tag:?} does not match workspace version {expected_tag:?}"
        )
        .into());
    }

    require_text(
        root,
        "scripts/install.sh",
        &format!("DEFAULT_VERSION='{expected_tag}'"),
    )?;
    require_text(root, "CHANGELOG.md", &format!("## [{version}]"))?;
    require_text(
        root,
        "docker-compose.production.yml",
        &format!("ghcr.io/tamedtornado/git-lfs-delta:{expected_tag}"),
    )?;
    require_text(
        root,
        ".env.example",
        &format!("ghcr.io/tamedtornado/git-lfs-delta:{expected_tag}"),
    )?;
    for path in ["CLIENT-README.md", "scripts/package-client.sh"] {
        if !root.join(path).is_file() {
            return Err(format!("required release file is missing: {path}").into());
        }
    }

    println!("release metadata is consistent for {expected_tag}");
    Ok(())
}

fn require_text(root: &Path, path: &str, expected: &str) -> Result<(), Box<dyn Error>> {
    let contents = fs::read_to_string(root.join(path))?;
    if contents.contains(expected) {
        Ok(())
    } else {
        Err(format!("{path} does not contain expected release value {expected:?}").into())
    }
}

fn dev(root: &Path) -> Result<(), Box<dyn Error>> {
    compose(root, &["up", "-d", "--wait", "postgres"])?;
    let state = root.join("target/dev");
    let storage = state.join("storage");
    let staging = state.join("staging");
    fs::create_dir_all(&storage)?;
    fs::create_dir_all(&staging)?;
    let storage_url = file_url(&storage);
    let staging = staging.to_string_lossy().into_owned();
    let environment = [
        ("GIT_LFS_DELTA_DATABASE_URL", DATABASE_URL),
        ("GIT_LFS_DELTA_BASE_URL", "http://127.0.0.1:8080/"),
        ("GIT_LFS_DELTA_STORAGE_URL", storage_url.as_str()),
        ("GIT_LFS_DELTA_AUTH_MODE", "development"),
        ("GIT_LFS_DELTA_DEV_TOKEN", DEVELOPMENT_TOKEN),
        ("GIT_LFS_DELTA_BIND", "127.0.0.1:8080"),
        ("GIT_LFS_DELTA_STAGING_DIR", staging.as_str()),
    ];
    command(
        root,
        cargo(),
        &[
            "run",
            "--locked",
            "--quiet",
            "-p",
            "git-lfs-delta-server",
            "--bin",
            "git-lfs-delta-admin",
            "--",
            "migrate",
        ],
        &environment,
    )?;
    println!("Git LFS Delta: http://127.0.0.1:8080");
    println!("development bearer token: {DEVELOPMENT_TOKEN}");
    println!("stop PostgreSQL later with `cargo dev-down`");
    command(
        root,
        cargo(),
        &[
            "run",
            "--locked",
            "-p",
            "git-lfs-delta-server",
            "--bin",
            "git-lfs-delta-server",
        ],
        &environment,
    )
}

fn ci(root: &Path) -> Result<(), Box<dyn Error>> {
    command(root, cargo(), &["fmt", "--all", "--", "--check"], &[])?;
    command(
        root,
        cargo(),
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        &[],
    )?;
    command(root, cargo(), &["test", "--workspace", "--locked"], &[])
}

fn compose(root: &Path, arguments: &[&str]) -> Result<(), Box<dyn Error>> {
    let mut command_arguments = vec!["compose", "-f", "docker-compose.test.yml"];
    command_arguments.extend_from_slice(arguments);
    command(root, "docker", &command_arguments, &[])
}

fn command(
    root: &Path,
    program: &str,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> Result<(), Box<dyn Error>> {
    println!("+ {program} {}", arguments.join(" "));
    let status = Command::new(program)
        .args(arguments)
        .envs(environment.iter().copied())
        .current_dir(root)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}").into())
    }
}

fn cargo() -> &'static str {
    option_env!("CARGO").unwrap_or("cargo")
}

fn workspace_root() -> Result<PathBuf, Box<dyn Error>> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask is not located under the workspace crates directory".into())
}

fn file_url(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    }
}
