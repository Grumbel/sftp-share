# TODO

Track outstanding work on sftp-share. Check items off as they land.

## High priority

- [x] Fix `resolve()` so CREATE/open of non-existent paths works (`--write`)
- [x] Cap SFTP `read` buffer size (prevent client-triggered OOM)
- [x] Open the canonical path after resolve (reduce symlink TOCTOU)

## Medium priority

- [x] Implement real `lstat` (use `symlink_metadata`, not `metadata`)
- [x] Warn on basename collisions in multi-path share mode
- [ ] Map I/O errors more precisely (`PermissionDenied` vs `NoSuchFile` vs `Failure`)

## Low priority

- [x] Constant-time password comparison
- [x] `--one-shot` only after a successful SFTP subsystem session
- [ ] Avoid embedding raw password in printed `sshpass` line when it contains shell-special chars
- [ ] Optional: include `.` / `..` in `readdir` for picky clients
- [ ] Optional: chunk large `readdir` responses

## Build / API verification

- [ ] `cargo build --release` against locked russh / russh-sftp versions
- [ ] Confirm `OpenFlags` variant names (`CREATE`/`TRUNCATE`/`EXCLUDE`)
- [ ] Confirm protocol struct field names if the lockfile moves

## Docs

- [x] `AGENTS.md` for contributors / agents
- [x] `TODO.md` (this file)
- [ ] Trim stale “not compiled against live crates” README note once build is verified
