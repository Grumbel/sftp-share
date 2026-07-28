//! sftp-share: a tiny, standalone SFTP server for sharing arbitrary files
//! and directories with virtual users. No root, no system users, no config
//! file. See README.md for usage.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::Parser;
use rand::Rng;
use russh::server::{Auth, Handler as SshHandler, Msg, Server as SshServerTrait, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode,
    Version,
};

// ---------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "sftp-share",
    version,
    about = "A tiny, standalone SFTP server for sharing arbitrary files and directories with virtual users"
)]
struct Cli {
    /// Files and/or directories to share. Nothing is shared unless you pass
    /// at least one path; use "." to share the current directory.
    paths: Vec<PathBuf>,

    /// Username required to log in.
    #[arg(long, default_value = "share")]
    user: String,

    /// Password required to log in. Randomly generated if not given.
    #[arg(long)]
    password: Option<String>,

    /// Port to listen on.
    #[arg(long, default_value_t = 2222)]
    port: u16,

    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0")]
    listen: String,

    /// Enable uploads (write access). Read-only by default.
    #[arg(long)]
    write: bool,

    /// Exit automatically after this duration (e.g. "30m", "2h").
    #[arg(long)]
    timeout: Option<String>,

    /// Exit after the first client disconnects.
    #[arg(long = "one-shot")]
    one_shot: bool,

    /// Verbose logging.
    #[arg(short, long)]
    verbose: bool,
}

// ---------------------------------------------------------------------
// Virtual filesystem
// ---------------------------------------------------------------------

/// How the virtual root `/` maps onto the real filesystem.
enum Root {
    /// `sftp-share .`: `/` *is* this real directory (flattened, no
    /// enclosing folder name). Never chosen automatically — the user must
    /// pass "." explicitly.
    Single(PathBuf),
    /// `sftp-share a b c`: `/` contains one named entry per shared path.
    Multi(Vec<(String, PathBuf)>),
}

enum Resolved {
    /// The synthetic root directory of a `Multi` share (not a real path).
    VirtualRoot,
    Path(PathBuf),
}

struct AppState {
    user: String,
    password: String,
    write_enabled: bool,
    root: Root,
}

impl AppState {
    /// Resolve a client-supplied absolute virtual path (e.g. "/photos/a.jpg")
    /// into a real path, refusing anything that would escape the shared
    /// directories.
    ///
    /// Returns a **canonical** path when the target exists. For not-yet-existing
    /// targets (CREATE), canonicalizes the parent and joins the final component
    /// so `--write` can create new files without failing the sandbox check.
    fn resolve(&self, virt_path: &str) -> Result<Resolved, StatusCode> {
        let comps: Vec<&str> = virt_path
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .collect();

        // We never allow ".." in a client path: every legitimate SFTP
        // client always sends normalized absolute paths, so this is not a
        // usability loss, only a safety margin.
        if comps.iter().any(|c| *c == "..") {
            return Err(StatusCode::PermissionDenied);
        }

        match &self.root {
            Root::Single(base) => {
                if comps.is_empty() {
                    let canon = base.canonicalize().map_err(|_| StatusCode::NoSuchFile)?;
                    return Ok(Resolved::Path(canon));
                }
                let mut p = base.clone();
                for c in &comps {
                    p.push(c);
                }
                let canon_base = base.canonicalize().map_err(|_| StatusCode::NoSuchFile)?;
                Self::sandbox_path(p, &canon_base)
            }
            Root::Multi(entries) => {
                if comps.is_empty() {
                    return Ok(Resolved::VirtualRoot);
                }
                let name = comps[0];
                let Some((_, target)) = entries.iter().find(|(n, _)| n == name) else {
                    return Err(StatusCode::NoSuchFile);
                };
                if comps.len() == 1 {
                    let canon = target.canonicalize().map_err(|_| StatusCode::NoSuchFile)?;
                    return Ok(Resolved::Path(canon));
                }
                let meta = std::fs::symlink_metadata(target).map_err(|_| StatusCode::NoSuchFile)?;
                if !meta.is_dir() {
                    return Err(StatusCode::NoSuchFile);
                }
                let mut p = target.clone();
                for c in &comps[1..] {
                    p.push(c);
                }
                let canon_base = target.canonicalize().map_err(|_| StatusCode::NoSuchFile)?;
                Self::sandbox_path(p, &canon_base)
            }
        }
    }

    /// Ensure `path` stays under `canon_base`. If `path` exists, return its
    /// canonical form; if not, canonicalize the parent and re-join the final
    /// component (needed for CREATE of new files).
    fn sandbox_path(path: PathBuf, canon_base: &std::path::Path) -> Result<Resolved, StatusCode> {
        match path.canonicalize() {
            Ok(canon) => {
                if !canon.starts_with(canon_base) {
                    return Err(StatusCode::PermissionDenied);
                }
                Ok(Resolved::Path(canon))
            }
            Err(_) => {
                let parent = path.parent().ok_or(StatusCode::NoSuchFile)?;
                let name = path.file_name().ok_or(StatusCode::NoSuchFile)?;
                let canon_parent = parent.canonicalize().map_err(|_| StatusCode::NoSuchFile)?;
                if !canon_parent.starts_with(canon_base) {
                    return Err(StatusCode::PermissionDenied);
                }
                Ok(Resolved::Path(canon_parent.join(name)))
            }
        }
    }
}

fn attrs_from_metadata(meta: &std::fs::Metadata) -> FileAttributes {
    let mut attrs = FileAttributes::default();
    attrs.size = Some(meta.len());

    let to_unix = |t: std::io::Result<SystemTime>| -> Option<u32> {
        t.ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32)
    };
    attrs.atime = to_unix(meta.accessed());
    attrs.mtime = to_unix(meta.modified());

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        attrs.uid = Some(meta.uid());
        attrs.gid = Some(meta.gid());
        attrs.permissions = Some(meta.mode());
    }
    #[cfg(not(unix))]
    {
        attrs.permissions = Some(if meta.is_dir() { 0o040755 } else { 0o100644 });
    }

    attrs
}

fn synthetic_dir_attrs() -> FileAttributes {
    let mut attrs = FileAttributes::default();
    attrs.size = Some(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;
    attrs.mtime = Some(now);
    attrs.atime = Some(now);
    attrs.permissions = Some(0o040755);
    attrs
}

fn longname(name: &str, attrs: &FileAttributes) -> String {
    let is_dir = attrs
        .permissions
        .map(|p| p & 0o170000 == 0o040000)
        .unwrap_or(false);
    let kind = if is_dir { 'd' } else { '-' };
    let size = attrs.size.unwrap_or(0);
    format!("{kind}rwxr-xr-x 1 share share {size:>10} {name}")
}

fn ok_status(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_string(),
        language_tag: "en-US".to_string(),
    }
}

/// Map std I/O errors to SFTP status codes instead of always using NoSuchFile.
fn io_to_status(err: std::io::Error) -> StatusCode {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::NotFound => StatusCode::NoSuchFile,
        ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        ErrorKind::AlreadyExists => StatusCode::Failure,
        ErrorKind::InvalidInput | ErrorKind::InvalidData => StatusCode::BadMessage,
        _ => StatusCode::Failure,
    }
}

// ---------------------------------------------------------------------
// SFTP protocol handler (one per SSH channel / client)
// ---------------------------------------------------------------------

enum OpenHandle {
    File(tokio::fs::File),
    Dir {
        entries: Vec<(String, FileAttributes)>,
        sent: bool,
    },
}

struct SftpSession {
    state: Arc<AppState>,
    handles: HashMap<String, OpenHandle>,
    next_handle: u64,
    verbose: bool,
}

impl SftpSession {
    fn new(state: Arc<AppState>, verbose: bool) -> Self {
        Self {
            state,
            handles: HashMap::new(),
            next_handle: 0,
            verbose,
        }
    }

    fn alloc_handle(&mut self) -> String {
        self.next_handle += 1;
        format!("h{}", self.next_handle)
    }

    fn log_access(&self, op: &str, path: &str) {
        if self.verbose {
            println!("  {op} {path}");
            tracing::debug!("{op} {path}");
        }
    }
}

impl russh_sftp::server::Handler for SftpSession {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let wants_write = pflags.contains(OpenFlags::WRITE)
            || pflags.contains(OpenFlags::CREATE)
            || pflags.contains(OpenFlags::TRUNCATE)
            || pflags.contains(OpenFlags::APPEND);

        if wants_write && !self.state.write_enabled {
            return Err(StatusCode::PermissionDenied);
        }

        self.log_access(if wants_write { "open-write" } else { "open" }, &filename);

        let path = match self.state.resolve(&filename)? {
            Resolved::Path(p) => p,
            Resolved::VirtualRoot => return Err(StatusCode::PermissionDenied),
        };

        let mut opts = tokio::fs::OpenOptions::new();
        opts.read(true);
        if wants_write {
            opts.write(true);
        }
        if pflags.contains(OpenFlags::CREATE) {
            opts.create(true);
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            opts.truncate(true);
        }
        if pflags.contains(OpenFlags::APPEND) {
            opts.append(true);
        }
        if pflags.contains(OpenFlags::EXCLUDE) {
            opts.create_new(true);
        }

        let file = opts.open(&path).await.map_err(io_to_status)?;
        let handle = self.alloc_handle();
        self.handles.insert(handle.clone(), OpenHandle::File(file));
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok_status(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        // Cap to avoid a malicious client requesting multi-GB buffers.
        const MAX_READ: u32 = 1024 * 1024;
        let len = len.min(MAX_READ);
        let Some(OpenHandle::File(file)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io_to_status)?;
        let mut buf = vec![0u8; len as usize];
        let n = file.read(&mut buf).await.map_err(io_to_status)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        if !self.state.write_enabled {
            return Err(StatusCode::PermissionDenied);
        }
        let Some(OpenHandle::File(file)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io_to_status)?;
        file.write_all(&data).await.map_err(io_to_status)?;
        Ok(ok_status(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.log_access("lstat", &path);
        match self.state.resolve(&path)? {
            Resolved::VirtualRoot => Ok(Attrs {
                id,
                attrs: synthetic_dir_attrs(),
            }),
            Resolved::Path(p) => {
                let meta = std::fs::symlink_metadata(&p).map_err(|_| StatusCode::NoSuchFile)?;
                Ok(Attrs {
                    id,
                    attrs: attrs_from_metadata(&meta),
                })
            }
        }
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let Some(OpenHandle::File(file)) = self.handles.get(&handle) else {
            return Err(StatusCode::Failure);
        };
        let meta = file.metadata().await.map_err(io_to_status)?;
        Ok(Attrs {
            id,
            attrs: attrs_from_metadata(&meta),
        })
    }

    async fn setstat(
        &mut self,
        _id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }

    async fn fsetstat(
        &mut self,
        _id: u32,
        _handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        self.log_access("opendir", &path);
        let mut entries = match self.state.resolve(&path)? {
            Resolved::VirtualRoot => {
                let Root::Multi(list) = &self.state.root else {
                    unreachable!("VirtualRoot only occurs with Root::Multi")
                };
                list.iter()
                    .filter_map(|(name, target)| {
                        std::fs::symlink_metadata(target)
                            .ok()
                            .map(|m| (name.clone(), attrs_from_metadata(&m)))
                    })
                    .collect::<Vec<_>>()
            }
            Resolved::Path(p) => {
                let meta = std::fs::symlink_metadata(&p).map_err(io_to_status)?;
                if !meta.is_dir() {
                    return Err(StatusCode::Failure);
                }
                let rd = std::fs::read_dir(&p).map_err(io_to_status)?;
                rd.filter_map(|e| e.ok())
                    .filter_map(|e| {
                        e.metadata()
                            .ok()
                            .map(|m| (e.file_name().to_string_lossy().to_string(), attrs_from_metadata(&m)))
                    })
                    .collect::<Vec<_>>()
            }
        };
        // Many SFTP clients expect "." and ".." in directory listings.
        let dot = synthetic_dir_attrs();
        entries.insert(0, ("..".to_string(), dot.clone()));
        entries.insert(0, (".".to_string(), dot));

        let handle = self.alloc_handle();
        self.handles.insert(
            handle.clone(),
            OpenHandle::Dir {
                entries,
                sent: false,
            },
        );
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let Some(OpenHandle::Dir { entries, sent }) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if *sent {
            return Err(StatusCode::Eof);
        }
        *sent = true;
        let files = entries
            .iter()
            .map(|(name, attrs)| File {
                filename: name.clone(),
                longname: longname(name, attrs),
                attrs: attrs.clone(),
            })
            .collect();
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        if !self.state.write_enabled {
            return Err(StatusCode::PermissionDenied);
        }
        self.log_access("remove", &filename);
        let Resolved::Path(p) = self.state.resolve(&filename)? else {
            return Err(StatusCode::PermissionDenied);
        };
        tokio::fs::remove_file(&p)
            .await
            .map_err(io_to_status)?;
        Ok(ok_status(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        if !self.state.write_enabled {
            return Err(StatusCode::PermissionDenied);
        }
        self.log_access("mkdir", &path);
        let Resolved::Path(p) = self.state.resolve(&path)? else {
            return Err(StatusCode::PermissionDenied);
        };
        tokio::fs::create_dir(&p)
            .await
            .map_err(io_to_status)?;
        Ok(ok_status(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        if !self.state.write_enabled {
            return Err(StatusCode::PermissionDenied);
        }
        self.log_access("rmdir", &path);
        let Resolved::Path(p) = self.state.resolve(&path)? else {
            return Err(StatusCode::PermissionDenied);
        };
        tokio::fs::remove_dir(&p)
            .await
            .map_err(io_to_status)?;
        Ok(ok_status(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let comps: Vec<&str> = path
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .collect();
        let normalized = format!("/{}", comps.join("/"));
        Ok(Name {
            id,
            files: vec![File {
                filename: normalized.clone(),
                longname: normalized,
                attrs: FileAttributes::default(),
            }],
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.log_access("stat", &path);
        match self.state.resolve(&path)? {
            Resolved::VirtualRoot => Ok(Attrs {
                id,
                attrs: synthetic_dir_attrs(),
            }),
            Resolved::Path(p) => {
                let meta = std::fs::metadata(&p).map_err(|_| StatusCode::NoSuchFile)?;
                Ok(Attrs {
                    id,
                    attrs: attrs_from_metadata(&meta),
                })
            }
        }
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        if !self.state.write_enabled {
            return Err(StatusCode::PermissionDenied);
        }
        self.log_access("rename", &format!("{oldpath} -> {newpath}"));
        let Resolved::Path(old) = self.state.resolve(&oldpath)? else {
            return Err(StatusCode::PermissionDenied);
        };
        let Resolved::Path(new) = self.state.resolve(&newpath)? else {
            return Err(StatusCode::PermissionDenied);
        };
        tokio::fs::rename(&old, &new)
            .await
            .map_err(io_to_status)?;
        Ok(ok_status(id))
    }

    async fn readlink(&mut self, _id: u32, _path: String) -> Result<Name, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }

    async fn symlink(
        &mut self,
        _id: u32,
        _linkpath: String,
        _targetpath: String,
    ) -> Result<Status, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }

    async fn extended(
        &mut self,
        _id: u32,
        _request: String,
        _data: Vec<u8>,
    ) -> Result<Packet, Self::Error> {
        Err(StatusCode::OpUnsupported)
    }
}

// ---------------------------------------------------------------------
// SSH-level plumbing: one channel/session per connecting client
// ---------------------------------------------------------------------

struct SshSession {
    state: Arc<AppState>,
    channels: HashMap<ChannelId, Channel<Msg>>,
    one_shot: bool,
    shutdown: Arc<tokio::sync::Notify>,
    peer: Option<SocketAddr>,
    verbose: bool,
    /// True once the client successfully started the SFTP subsystem.
    /// Used so `--one-shot` does not fire on unrelated channel closes.
    sftp_started: bool,
}

#[async_trait::async_trait]
impl SshHandler for SshSession {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Reject {
            proceed_with_methods: Some(russh::MethodSet::PASSWORD),
        })
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: &russh_keys::key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Reject {
            proceed_with_methods: Some(russh::MethodSet::PASSWORD),
        })
    }

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        let peer = self
            .peer
            .map(|a| a.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        // Constant-time-ish compare so password length/content timing is less
        // of a side channel. Still not a hardened login service.
        let user_ok = const_eq(user.as_bytes(), self.state.user.as_bytes());
        let pass_ok = const_eq(password.as_bytes(), self.state.password.as_bytes());
        if user_ok && pass_ok {
            println!("client connected from {peer} as user `{user}`");
            tracing::info!("client connected from {peer} as user `{user}`");
            Ok(Auth::Accept)
        } else {
            println!("auth failed from {peer} (user `{user}`)");
            tracing::warn!("auth failed from {peer} (user `{user}`)");
            Ok(Auth::Reject {
                proceed_with_methods: Some(russh::MethodSet::PASSWORD),
            })
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.channels.insert(channel.id(), channel);
        Ok(true)
    }

    /// Client finished sending on this channel (e.g. `sftp` `exit`).
    /// We must close our side or the client hangs waiting for channel close.
    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // russh 0.44: close() is infallible (void). Newer versions return Result.
        session.close(channel);
        Ok(())
    }

    async fn channel_close(
        &mut self,
        _channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let peer = self
            .peer
            .map(|a| a.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!("client disconnected from {peer}");
        tracing::info!("client disconnected from {peer}");
        if self.one_shot && self.sftp_started {
            self.shutdown.notify_waiters();
        }
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col_width: u32,
        _row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel);
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel);
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel);
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(channel_id);
            return Ok(());
        }
        let Some(channel) = self.channels.remove(&channel_id) else {
            session.channel_failure(channel_id);
            return Ok(());
        };
        session.channel_success(channel_id);
        self.sftp_started = true;

        // russh_sftp::server::run spawns its own task and returns immediately.
        // Channel teardown is driven by channel_eof / channel_close above.
        let state = self.state.clone();
        let verbose = self.verbose;
        let sftp = SftpSession::new(state, verbose);
        russh_sftp::server::run(channel.into_stream(), sftp).await;

        Ok(())
    }
}

struct Server {
    state: Arc<AppState>,
    one_shot: bool,
    shutdown: Arc<tokio::sync::Notify>,
    verbose: bool,
}

impl SshServerTrait for Server {
    type Handler = SshSession;

    fn new_client(&mut self, addr: Option<SocketAddr>) -> SshSession {
        if let Some(a) = addr {
            println!("incoming connection from {a}");
            tracing::info!("incoming connection from {a}");
        }
        SshSession {
            state: self.state.clone(),
            channels: HashMap::new(),
            one_shot: self.one_shot,
            shutdown: self.shutdown.clone(),
            peer: addr,
            verbose: self.verbose,
            sftp_started: false,
        }
    }
}

// ---------------------------------------------------------------------
// main
// ---------------------------------------------------------------------

fn generate_password() -> String {
    const CHARS: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz";
    let mut rng = rand::thread_rng();
    let mut group = |n: usize| -> String {
        (0..n)
            .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
            .collect()
    };
    format!("{}-{}-{}", group(4), group(3), group(4))
}

/// Best-effort constant-time equality for short byte strings (usernames / passwords).
fn const_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Single-quote a string for safe embedding in a shell one-liner (`'foo'\''bar'`).
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose {
        "sftp_share=debug,russh=info"
    } else {
        "sftp_share=info,russh=warn"
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .init();

    let password = cli.password.clone().unwrap_or_else(generate_password);

    if cli.paths.is_empty() {
        anyhow::bail!(
            "no paths given — nothing will be shared automatically.\n\
             Pass one or more files/directories, or use \".\" to share the current directory:\n\
             \n  sftp-share .\n  sftp-share report.pdf photos/ README.md\n"
        );
    }

    // A single literal "." means "share the current directory's contents
    // directly at the virtual root" (flattened), matching `python -m
    // http.server`-style behavior. Anything else — including "." combined
    // with other paths — is treated as a named top-level entry per path.
    let root = if cli.paths.len() == 1 && cli.paths[0] == PathBuf::from(".") {
        Root::Single(std::env::current_dir()?)
    } else {
        let mut entries = Vec::new();
        let mut seen_names = std::collections::HashSet::new();
        for p in &cli.paths {
            let canon = p
                .canonicalize()
                .map_err(|e| anyhow::anyhow!("cannot access {}: {}", p.display(), e))?;
            let name = canon
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "root".to_string());
            if !seen_names.insert(name.clone()) {
                eprintln!(
                    "warning: multiple shared paths map to the same name `{name}` \
                     — only the first is reachable under /{name}"
                );
            }
            entries.push((name, canon));
        }
        Root::Multi(entries)
    };

    let state = Arc::new(AppState {
        user: cli.user.clone(),
        password: password.clone(),
        write_enabled: cli.write,
        root,
    });

    let key_pair =
        russh_keys::key::KeyPair::generate_ed25519().expect("failed to generate host key");

    let config = Arc::new(russh::server::Config {
        // Only password auth is supported — don't advertise publickey etc.
        methods: russh::MethodSet::PASSWORD,
        // Constant-time delay for failed password attempts (anti-timing).
        auth_rejection_time: Duration::from_secs(1),
        // OpenSSH always probes with "none" first; reject that instantly so the
        // password prompt appears without a multi-second wait.
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![key_pair],
        ..Default::default()
    });

    let listen_ip: IpAddr = cli
        .listen
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid --listen address: {}", cli.listen))?;
    let addr = SocketAddr::new(listen_ip, cli.port);

    let display_ip = if listen_ip.is_unspecified() {
        local_ip_address::local_ip()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|_| "127.0.0.1".to_string())
    } else {
        listen_ip.to_string()
    };

    let host_key_opts = "-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null";

    println!("Serving:\n");
    println!("  sftp://{}@{}:{}/", cli.user, display_ip, cli.port);
    println!("\n  Password: {}\n", password);
    println!("Connect with:\n");
    println!(
        "  sftp -P {port} {opts} {user}@{host}\n",
        port = cli.port,
        opts = host_key_opts,
        user = cli.user,
        host = display_ip,
    );
    println!(
        "  sshpass -p {pass} sftp -P {port} {opts} {user}@{host}\n",
        pass = shell_single_quote(&password),
        port = cli.port,
        opts = host_key_opts,
        user = cli.user,
        host = display_ip,
    );
    println!(
        "  sshfs {user}@{host}:/ ./mount -p {port} -o {opts}\n",
        user = cli.user,
        host = display_ip,
        port = cli.port,
        opts = "StrictHostKeyChecking=no,UserKnownHostsFile=/dev/null",
    );
    println!(
        "  (the host key is generated fresh in memory every time sftp-share\n\
         \x20  starts, so it's different on every run — the options above skip\n\
         \x20  host-key verification and known_hosts bookkeeping entirely, which\n\
         \x20  is fine for a throwaway share like this but would be a bad habit\n\
         \x20  for a server you expect to trust long-term)\n"
    );
    println!("Press Ctrl-C to stop.\n");

    let shutdown = Arc::new(tokio::sync::Notify::new());

    let mut server = Server {
        state: state.clone(),
        one_shot: cli.one_shot,
        shutdown: shutdown.clone(),
        verbose: cli.verbose,
    };

    let one_shot = cli.one_shot;
    let timeout_spec = cli.timeout.clone();

    tokio::select! {
        res = server.run_on_address(config, addr) => {
            res?;
        }
        _ = shutdown.notified(), if one_shot => {
            tracing::info!("client disconnected, exiting (--one-shot)");
        }
        _ = async {
            match &timeout_spec {
                Some(t) => {
                    let dur = humantime::parse_duration(t)
                        .unwrap_or_else(|_| panic!("invalid --timeout duration: {t}"));
                    tokio::time::sleep(dur).await;
                }
                None => std::future::pending::<()>().await,
            }
        } => {
            tracing::info!("timeout reached, exiting");
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("received Ctrl-C, exiting");
        }
    }

    Ok(())
}
