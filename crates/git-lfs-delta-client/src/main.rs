//! Git LFS Delta command-line entrypoint.

use std::{
    error::Error,
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::SystemTime,
};

use clap::{Parser, Subcommand, ValueEnum};
use directories::ProjectDirs;
use git_lfs_delta::{HttpBackend, run_transfer_protocol};

#[derive(Parser)]
#[command(version, about = "Content-defined Git LFS transfer agent")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Runs the line-delimited Git LFS custom-transfer process.
    Transfer,
    /// Configures this executable as a Git LFS custom transfer agent.
    Install {
        /// Git configuration scope to modify.
        #[arg(long, value_enum, default_value_t = Scope::Local)]
        scope: Scope,
    },
    /// Removes this executable's custom-transfer configuration.
    Uninstall {
        /// Git configuration scope to modify.
        #[arg(long, value_enum, default_value_t = Scope::Local)]
        scope: Scope,
    },
    /// Configures the repository's LFS endpoint.
    Configure {
        /// Full repository LFS base URL ending in `/info/lfs`.
        #[arg(long)]
        url: String,
        /// Git configuration scope to modify.
        #[arg(long, value_enum, default_value_t = Scope::Local)]
        scope: Scope,
    },
    /// Shows the effective transfer-agent and LFS endpoint configuration.
    Status,
    /// Verifies Git, Git LFS, configuration, and cache access.
    Doctor,
    /// Manages the verified local chunk cache.
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Scope {
    Local,
    Global,
}

#[derive(Subcommand)]
enum CacheCommands {
    /// Prints the number and total bytes of cached chunks.
    Stats,
    /// Removes oldest chunks until the cache is at or below the byte limit.
    Prune {
        /// Maximum retained cache size in bytes.
        #[arg(long)]
        max_bytes: u64,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if is_broken_pipe(error.as_ref()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("git-lfs-delta: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Transfer => {
            let backend = HttpBackend::new(cache_root()?)?;
            run_transfer_protocol(io::stdin().lock(), io::stdout().lock(), &backend)?;
        }
        Commands::Install { scope } => install(scope)?,
        Commands::Uninstall { scope } => uninstall(scope)?,
        Commands::Configure { url, scope } => configure(scope, &url)?,
        Commands::Status => status()?,
        Commands::Doctor => doctor()?,
        Commands::Cache { command } => match command {
            CacheCommands::Stats => {
                let (files, bytes) = cache_entries(&cache_root()?)?
                    .into_iter()
                    .fold((0_u64, 0_u64), |(files, bytes), entry| {
                        (files + 1, bytes + entry.size)
                    });
                write_output(format_args!("{files} chunks, {bytes} bytes"))?;
            }
            CacheCommands::Prune { max_bytes } => prune_cache(&cache_root()?, max_bytes)?,
        },
    }
    Ok(())
}

fn install(scope: Scope) -> Result<(), Box<dyn std::error::Error>> {
    require_command("git", &["--version"])?;
    require_command("git-lfs", &["version"])?;
    // Keep the launch path in its ordinary platform form. On Windows,
    // `fs::canonicalize` produces a `\\?\` verbatim path which Git LFS cannot
    // reliably launch as a custom transfer process. `doctor` canonicalizes
    // both sides when it compares paths, so symlink equivalence is still safe.
    let executable = std::env::current_exe()?;
    let flag = match scope {
        Scope::Local => "--local",
        Scope::Global => "--global",
    };
    git_config(
        flag,
        "lfs.customtransfer.cdc.path",
        &executable.to_string_lossy(),
    )?;
    git_config(flag, "lfs.customtransfer.cdc.args", "transfer")?;
    git_config(flag, "lfs.customtransfer.cdc.concurrent", "true")?;
    write_output(format_args!(
        "configured Git LFS Delta custom transfer agent ({flag})"
    ))?;
    Ok(())
}

fn uninstall(scope: Scope) -> Result<(), Box<dyn std::error::Error>> {
    let flag = match scope {
        Scope::Local => "--local",
        Scope::Global => "--global",
    };
    for key in [
        "lfs.customtransfer.cdc.path",
        "lfs.customtransfer.cdc.args",
        "lfs.customtransfer.cdc.concurrent",
    ] {
        let status = Command::new("git")
            .args(["config", flag, "--unset-all", key])
            .status()?;
        if !status.success() && status.code() != Some(5) {
            return Err(format!("git config could not remove {key}").into());
        }
    }
    write_output(format_args!(
        "removed Git LFS Delta custom transfer agent ({flag})"
    ))?;
    Ok(())
}

fn configure(scope: Scope, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let parsed = url::Url::parse(url)?;
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.path().ends_with("/info/lfs") {
        return Err("LFS URL must be HTTP(S) and end in /info/lfs".into());
    }
    let flag = match scope {
        Scope::Local => "--local",
        Scope::Global => "--global",
    };
    git_config(flag, "lfs.url", url)?;
    write_output(format_args!("configured LFS endpoint ({flag}): {url}"))?;
    Ok(())
}

fn status() -> Result<(), Box<dyn std::error::Error>> {
    for key in [
        "lfs.url",
        "lfs.customtransfer.cdc.path",
        "lfs.customtransfer.cdc.args",
        "lfs.customtransfer.cdc.concurrent",
    ] {
        let output = Command::new("git")
            .args(["config", "--get", key])
            .output()?;
        let value = if output.status.success() {
            String::from_utf8(output.stdout)?.trim().to_owned()
        } else {
            "<unset>".into()
        };
        write_output(format_args!("{key}={value}"))?;
    }
    Ok(())
}

fn doctor() -> Result<(), Box<dyn std::error::Error>> {
    require_command("git", &["--version"])?;
    require_command("git-lfs", &["version"])?;
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let configured = git_config_value("lfs.customtransfer.cdc.path")?
        .ok_or("Git LFS Delta transfer agent is not registered")?;
    let configured_path = fs::canonicalize(&configured).map_err(|error| {
        format!("configured transfer path {configured:?} is unavailable: {error}")
    })?;
    if configured_path != executable {
        return Err(format!(
            "configured transfer path {} does not match this executable {}; rerun install",
            configured_path.display(),
            executable.display()
        )
        .into());
    }
    let cache = cache_root()?;
    fs::create_dir_all(&cache)?;
    let probe = tempfile::NamedTempFile::new_in(&cache)?;
    drop(probe);
    write_output(format_args!("Git LFS Delta: {}", env!("CARGO_PKG_VERSION")))?;
    write_output(format_args!("executable: {}", executable.display()))?;
    write_output(format_args!(
        "transfer registration: {}",
        configured_path.display()
    ))?;
    write_output(format_args!("Git: ok"))?;
    write_output(format_args!("Git LFS: ok"))?;
    write_output(format_args!("cache: {} (writable)", cache.display()))?;
    Ok(())
}

fn git_config_value(key: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(["config", "--get", key])
        .output()?;
    if output.status.success() {
        return Ok(Some(String::from_utf8(output.stdout)?.trim().to_owned()));
    }
    if output.status.code() == Some(1) {
        return Ok(None);
    }
    Err(format!("git config could not read {key}").into())
}

fn git_config(scope: &str, key: &str, value: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("git")
        .args(["config", scope, key, value])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("git config failed for {key}").into())
    }
}

fn require_command(program: &str, arguments: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new(program).args(arguments).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} is installed but returned a failure").into())
    }
}

fn cache_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    ProjectDirs::from("ai", "TamedTornado", "git-lfs-delta")
        .map(|directories| directories.cache_dir().to_path_buf())
        .ok_or_else(|| "operating system did not provide a cache directory".into())
}

struct CacheEntry {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

fn cache_entries(root: &Path) -> io::Result<Vec<CacheEntry>> {
    let mut pending = vec![root.join("chunks")];
    let mut entries = Vec::new();
    while let Some(directory) = pending.pop() {
        let children = match fs::read_dir(directory) {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for child in children {
            let child = child?;
            let metadata = child.metadata()?;
            if metadata.is_dir() {
                pending.push(child.path());
            } else if metadata.is_file() {
                entries.push(CacheEntry {
                    path: child.path(),
                    size: metadata.len(),
                    modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
        }
    }
    Ok(entries)
}

fn prune_cache(root: &Path, maximum: u64) -> io::Result<()> {
    let mut entries = cache_entries(root)?;
    entries.sort_by_key(|entry| entry.modified);
    let mut total: u64 = entries.iter().map(|entry| entry.size).sum();
    let mut removed = 0_u64;
    for entry in entries {
        if total <= maximum {
            break;
        }
        fs::remove_file(entry.path)?;
        total = total.saturating_sub(entry.size);
        removed += 1;
    }
    write_output(format_args!(
        "removed {removed} chunks; cache now uses {total} bytes"
    ))?;
    Ok(())
}

fn write_output(arguments: fmt::Arguments<'_>) -> io::Result<()> {
    writeln!(io::stdout().lock(), "{arguments}")
}

fn is_broken_pipe(mut error: &(dyn Error + 'static)) -> bool {
    loop {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
        {
            return true;
        }
        match error.source() {
            Some(source) => error = source,
            None => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_broken_pipe;
    use std::io;

    #[test]
    fn closed_output_pipe_is_a_successful_cli_termination() {
        let error = io::Error::from(io::ErrorKind::BrokenPipe);
        assert!(is_broken_pipe(&error));
    }
}
