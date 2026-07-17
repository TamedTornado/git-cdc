//! Git-CDC command-line entrypoint.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::SystemTime,
};

use clap::{Parser, Subcommand, ValueEnum};
use directories::ProjectDirs;
use git_cdc::{HttpBackend, run_transfer_protocol};

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
        Err(error) => {
            eprintln!("git-cdc: {error}");
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
        Commands::Doctor => doctor()?,
        Commands::Cache { command } => match command {
            CacheCommands::Stats => {
                let (files, bytes) = cache_entries(&cache_root()?)?
                    .into_iter()
                    .fold((0_u64, 0_u64), |(files, bytes), entry| {
                        (files + 1, bytes + entry.size)
                    });
                println!("{files} chunks, {bytes} bytes");
            }
            CacheCommands::Prune { max_bytes } => prune_cache(&cache_root()?, max_bytes)?,
        },
    }
    Ok(())
}

fn install(scope: Scope) -> Result<(), Box<dyn std::error::Error>> {
    require_command("git", &["--version"])?;
    require_command("git-lfs", &["version"])?;
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
    println!("configured Git-CDC custom transfer agent ({flag})");
    Ok(())
}

fn doctor() -> Result<(), Box<dyn std::error::Error>> {
    require_command("git", &["--version"])?;
    require_command("git-lfs", &["version"])?;
    let cache = cache_root()?;
    fs::create_dir_all(&cache)?;
    let probe = tempfile::NamedTempFile::new_in(&cache)?;
    drop(probe);
    println!("Git: ok");
    println!("Git LFS: ok");
    println!("cache: {} (writable)", cache.display());
    Ok(())
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
    ProjectDirs::from("ai", "TamedTornado", "git-cdc")
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
    println!("removed {removed} chunks; cache now uses {total} bytes");
    Ok(())
}
