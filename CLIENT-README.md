# Git LFS Delta client

This archive contains the native Git LFS custom-transfer client. Install it with
the release's `install.sh`, or place `git-lfs-delta` at a stable path and run:

```console
git-lfs-delta install --scope global
```

Each repository opts in by setting its Git LFS endpoint:

```console
git-lfs-delta configure --scope local --url https://HOST/OWNER/REPOSITORY/info/lfs
```

Run `git-lfs-delta doctor` to verify Git, Git LFS, cache access, and transfer
registration. The software is dual-licensed under MIT or Apache-2.0.

