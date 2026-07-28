# AGENTS.md

Guidance for humans and coding agents working on **sftp-share**.

## What this is

A tiny, standalone SFTP server for ad-hoc sharing of files/directories with a virtual user. No root, no system accounts, no config file. Built on `russh` + `russh-sftp`.

Primary entry point: `src/main.rs` (single binary).

## Design constraints (do not casually break)

1. **Nothing is shared unless the user passes at least one path.** Empty args → error, not “share cwd”.
2. **Read-only by default.** Writes only with `--write`.
3. **Password auth only.** Reject publickey / none; no shell, PTY, or exec.
4. **Ephemeral in-memory host key** every run — no state directory.
5. **Path sandboxing:** client paths must not escape shared roots via `..` or symlink tricks. Prefer canonicalize + prefix checks; open canonical paths where possible.
6. **Zero-config UX:** print connection instructions (sftp / sshpass / sshfs) and a generated password on startup.

## Architecture sketch

| Piece | Role |
|--------|------|
| `Cli` | clap flags: paths, user, password, port, listen, write, timeout, one-shot, verbose |
| `Root` / `AppState` / `resolve` | Virtual FS mapping (`Single` for `.`, `Multi` for named top-level entries) |
| `SftpSession` | `russh_sftp::server::Handler` — open/read/write/dir ops |
| `SshSession` | `russh::server::Handler` — auth, channel lifecycle, subsystem handoff |
| `Server` | `russh::server::Server` — accepts clients, builds `SshSession` |

SFTP runs via `channel.into_stream()` → `russh_sftp::server::run`. That `run` typically spawns internally and returns quickly; **channel teardown** is driven by `channel_eof` (must `session.close`) and `channel_close`.

## Working rules

- Prefer small, reviewable diffs. One concern per change when possible.
- Keep the binary dependency set lean; this is a share tool, not a framework.
- After behavior changes, update `TODO.md` checkboxes and README only when the user-facing contract changes.
- Do not log passwords at info/debug. The startup banner intentionally prints the password for the operator; that is the exception.
- Verbose (`-v`) may log virtual paths being accessed; never log file contents.
- When touching `resolve` or open paths, think about: non-existent create targets, trailing slashes, symlink escape, basename collisions in `Multi` mode.
- **Always end chat replies that made code or doc changes with a git-style commit message** covering that turn’s work (subject ≤50–72 chars, optional body with bullets). Do this even when the user did not ask for a commit; they can copy-paste it.

## Common pitfalls

- **`canonicalize` requires the path to exist.** For CREATE, canonicalize the parent and join the final component.
- **OpenSSH probes with `none` auth first.** Use `auth_rejection_time_initial: Some(Duration::ZERO)` or the password prompt appears delayed.
- **Missing `channel_eof` → `session.close`** makes `sftp` `exit` hang on the client.
- **russh API drift:** 0.44 vs newer versions differ on `channel_open_session` signatures, `Session::close` return type, and key types. Match `Cargo.lock`.
- **`OpenFlags` names** in russh-sftp may be `CREATE`/`TRUNCATE`/`EXCLUDE` or shorter forms — check the crate you resolve.

## Build

```bash
cargo build --release
# or
nix build
nix run . -- .
```

Internet may be required the first time crates are fetched. `Cargo.lock` must stay committed for the Nix flake (`cargoLock.lockFile`).

## Testing checklist (manual)

1. `sftp-share .` — list, get a file, `exit` returns immediately (no hang).
2. Password prompt appears quickly (no multi-second delay).
3. Without `--write`: put/mkdir fail with permission denied.
4. With `--write`: create a new file, mkdir, rename, remove.
5. `sftp-share a b` — both appear under `/` by basename; collision warns.
6. `-v` prints access lines; connect/disconnect always print.
7. `--one-shot` exits only after a real SFTP session ends, not on a random channel close.

## Out of scope (unless explicitly requested)

- Public-key auth, config files, persistent host keys
- Symlink create/read as a feature
- Full POSIX ACL / chown / setstat
- Multi-user virtual accounts
