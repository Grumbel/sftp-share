# sftp-share

A tiny, standalone SFTP server for sharing arbitrary files and directories
with virtual users. No root, no system users, no config file.

```bash
sftp-share .                         # share the current directory
sftp-share report.pdf photos/ README.md
```

Nothing is shared unless you explicitly say so — running `sftp-share` with
no arguments prints an error rather than silently sharing the current
directory.

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
  (named after its basename); `sftp-share .` is a special case that shares
  the current directory's contents flattened directly at `/`, with no
  enclosing folder. Nothing is ever shared automatically — at least one
  path argument is required. Path resolution canonicalizes and checks
  prefixes to prevent escaping the shared paths via `..` or symlink
  shenanigans.
- Read-only by default; `--write` enables `open`-for-write, `mkdir`,
  `rmdir`, `remove`, and `rename`. Symlink creation/reading is never
  supported.

## A note on dependencies

Pinned to `russh` 0.44.x and `russh-sftp` 2.x via `Cargo.lock`. A recent
stable Rust/Cargo (1.78+) is recommended; older toolchains may reject the
lockfile format or transitive crates that use newer editions.

If `cargo build` reports missing `OpenFlags` variants or protocol struct
fields after a dependency bump, check the resolved crate docs — those names
have shifted between minor releases (`CREATE` vs `CREAT`, etc.).
