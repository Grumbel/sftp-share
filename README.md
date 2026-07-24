# sftp-share

A tiny, standalone SFTP server for sharing arbitrary files and directories
with virtual users. No root, no system users, no config file.

```bash
sftp-share                          # share the current directory
sftp-share report.pdf photos/ README.md
```

## Options

```
--user NAME          Username (default: share)
--password PASS      Password (default: randomly generated)
--port PORT          Port to listen on (default: 2222)
--listen ADDR        Address to listen on (default: 0.0.0.0)
--write              Enable uploads (default: read-only)
--timeout 30m        Exit after a duration
--one-shot           Exit after first client disconnects
-v, --verbose        Verbose logging
```

## Building

### With Cargo

```bash
cargo build --release
```

### With Nix

```bash
nix build
./result/bin/sftp-share
```

or run directly:

```bash
nix run . -- report.pdf photos/
```

`flake.nix` uses `rustPlatform.buildRustPackage` with `cargoLock.lockFile`,
which needs a `Cargo.lock` committed in the repo. If it isn't present yet,
generate it once with `cargo generate-lockfile` (or just `cargo build`).

## Design notes

- Built on [`russh`](https://crates.io/crates/russh) and
  [`russh-sftp`](https://crates.io/crates/russh-sftp) — both pure Rust, no
  dependency on the system's OpenSSH/libssh.
- The SSH host key is an ephemeral in-memory Ed25519 key generated fresh on
  every startup — nothing is written to disk, so there's no config/state
  directory to manage or clean up. This means the host key fingerprint
  changes every run; that's expected for a zero-config sharing tool, but it
  does mean SFTP clients will complain about an "unknown host key" every
  time (there's nothing to pin against).
- Only password authentication is accepted; public-key and "none" auth are
  always rejected.
- Shell/exec/PTY requests are refused — this server does nothing but SFTP.
- The virtual filesystem is intentionally simple: when you pass explicit
  paths, each one appears as a single named entry directly under `/`
  (named after its basename); with no arguments, `/` *is* the current
  directory. Path resolution canonicalizes and checks prefixes to prevent
  escaping the shared paths via `..` or symlink shenanigans.
- Read-only by default; `--write` enables `open`-for-write, `mkdir`,
  `rmdir`, `remove`, and `rename`. Symlink creation/reading is never
  supported.

## A note on this build

This code was written to match the documented shape of `russh` 0.44.x /
`russh-sftp` 2.x, but it was **not compiled against live crates** in the
environment that produced it (no network access at generation time). Expect
to run `cargo build` once and fix a few likely small API mismatches, most
plausibly around:

- `OpenFlags` variant names (`CREATE`/`CREAT`, `TRUNCATE`/`TRUNC`,
  `EXCLUDE`/`EXCL`) — check `russh_sftp::protocol::OpenFlags`.
- The exact field names on `Status`, `FileAttributes`, `Handle`, `Name`,
  `Data`, `Attrs`, `Version` in `russh_sftp::protocol`.
- `Channel::into_stream()` — added to `russh` specifically to bridge SSH
  channels into `AsyncRead + AsyncWrite` for subsystems like SFTP; confirm
  it exists in whatever `russh` version Cargo resolves.
- `russh::server::Config` field names (`keys`, `auth_rejection_time`, etc.)
  and the exact signature of `russh::server::run`.

None of these are structural problems — they're the kind of thing the
compiler will point at directly, one error at a time.
