# TODO

Track outstanding work on sftp-share. Check items off as they land.

## High priority

- [x] Fix `resolve()` so CREATE/open of non-existent paths works (`--write`)
- [x] Cap SFTP `read` buffer size (prevent client-triggered OOM)
- [x] Open the canonical path after resolve (reduce symlink TOCTOU)

## Medium priority

- [x] Implement real `lstat` (use `symlink_metadata`, not `metadata`)
- [x] Warn on basename collisions in multi-path share mode
- [x] Map I/O errors more precisely (`PermissionDenied` vs `NoSuchFile` vs `Failure`)

## Low priority

- [x] Constant-time password comparison
- [x] `--one-shot` only after a successful SFTP subsystem session
- [x] Avoid embedding raw password in printed `sshpass` line when it contains shell-special chars
- [x] Include `.` / `..` in `readdir` for picky clients
- [x] Chunk large `readdir` responses (batches of 100)

## Build / API verification

- [ ] `cargo build --release` on a modern toolchain (1.78+); this sandbox has Cargo 1.75 and cannot resolve edition2024 crates (e.g. clap 4.6)
- [x] Confirm `OpenFlags` variant names (`CREATE`/`TRUNCATE`/`EXCLUDE`) — matches russh-sftp 2.x docs
- [x] Confirm protocol struct fields against docs (`Status`, `Handle`, `Name`, `Data`, `Attrs`, `FileAttributes`, `Version`) — aligned with 2.x; re-check after any lockfile bump + real build

## Docs

- [x] `AGENTS.md` for contributors / agents
- [x] `TODO.md` (this file)
- [x] Trim stale “not compiled against live crates” README note
