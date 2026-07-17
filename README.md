# Git-CDC

Git-CDC is an open, forge-neutral, content-defined-chunking backend for Git
Large File Storage. Existing Git LFS pointer files and clients remain valid;
installing the native `git-cdc` custom transfer agent additionally allows a
client to upload and download only chunks it does not already share with the
server.

The project is currently under active development toward its first usable
beta. The design is recorded in [the project plan](docs/PROJECT_PLAN.md).

## Compatibility promise

- Git remains the version-control system.
- Repositories retain standard Git LFS SHA-256 pointer files.
- Stock Git LFS clients use the standard basic transfer path.
- Git-CDC-aware clients negotiate a chunk-aware transfer path.
- Forgejo is the first reference integration, not a core dependency.

## Planned components

- `git-cdc`: a native Git LFS custom-transfer agent for Windows, macOS, and
  Linux.
- `git-cdc-server`: a Linux-first LFS and CDC service backed by PostgreSQL and
  pluggable object storage.

## License

Git-CDC is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.
