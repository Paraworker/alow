use rustix::fs;
use std::{
    borrow::Cow,
    env, io,
    os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
    path::{Path, PathBuf},
};
use thiserror::Error;
use tokio::net::{UnixListener, UnixStream};

/// Errors returned by [`WaylandSocket`].
#[derive(Debug, Error)]
pub enum SocketError {
    #[error("no available socket candidates")]
    NoAvailableSocket,
    #[error("XDG_RUNTIME_DIR not set or invalid")]
    RuntimeDirInvalid,
    #[error("could not open or create lock file: {0}")]
    LockOpen(#[source] io::Error),
    #[error("could not acquire file lock: {0}")]
    LockAcquire(#[source] io::Error),
    #[error("could not bind to socket: {0}")]
    Bind(#[source] io::Error),
    #[error("could not accept incoming connection: {0}")]
    Accept(#[source] io::Error),
}

/// Wayland server socket.
#[derive(Debug)]
pub struct WaylandSocket {
    listener: UnixListener,

    name: String,
    bind_path: PathBuf,
    lock_path: PathBuf,

    _lock: OwnedFd,
}

impl WaylandSocket {
    /// Automatically binds to an available socket.
    ///
    /// The socket will be created under the `XDG_RUNTIME_DIR`.
    pub fn auto() -> Result<Self, SocketError> {
        // Skip `wayland-0`
        Self::with_candidates((1..32).map(|i| format!("wayland-{i}").into()))
    }

    /// Attempts to bind to a socket from a set of names.
    ///
    /// The socket will be created under the `XDG_RUNTIME_DIR`.
    pub fn with_candidates<'a, I>(candidates: I) -> Result<Self, SocketError>
    where
        I: IntoIterator<Item = Cow<'a, str>>,
    {
        Self::with_candidates_in_dir(&xdg_runtime_dir()?, candidates)
    }

    /// Binds to a socket with the given name.
    ///
    /// The socket will be created under the `XDG_RUNTIME_DIR`.
    pub fn with_name(name: Cow<'_, str>) -> Result<Self, SocketError> {
        Self::with_name_in_dir(&xdg_runtime_dir()?, name)
    }

    /// Attempts to bind to a socket from a set of names in the given directory.
    pub fn with_candidates_in_dir<'a, I>(dir: &Path, candidates: I) -> Result<Self, SocketError>
    where
        I: IntoIterator<Item = Cow<'a, str>>,
    {
        for name in candidates {
            match Self::with_name_in_dir(dir, name) {
                // Successfully bound to a socket, return.
                Ok(socket) => return Ok(socket),

                // Failed to acquire lock, try the next one.
                Err(SocketError::LockAcquire(_)) => continue,

                // Other errors, abort.
                Err(err) => return Err(err),
            }
        }

        Err(SocketError::NoAvailableSocket)
    }

    /// Binds to a socket in the given directory with the given name.
    pub fn with_name_in_dir(dir: &Path, name: Cow<'_, str>) -> Result<Self, SocketError> {
        // Build paths
        let (bind_path, lock_path) = build_paths(dir, name.as_ref());

        // Try to lock
        let _lock = lock_file(&lock_path)?;

        // Remove leftover socket file if it exists
        let _ = fs::unlink(&bind_path);

        // Bind and listen
        let listener = UnixListener::bind(&bind_path).map_err(SocketError::Bind)?;

        Ok(WaylandSocket {
            listener,
            name: name.into(),
            bind_path,
            lock_path,
            _lock,
        })
    }

    /// Returns the name of the socket.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Accepts a new connection.
    pub async fn accept(&self) -> Result<UnixStream, SocketError> {
        let (stream, _) = self.listener.accept().await.map_err(SocketError::Accept)?;
        Ok(stream)
    }
}

impl AsRawFd for WaylandSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }
}

impl AsFd for WaylandSocket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.listener.as_fd()
    }
}

impl Drop for WaylandSocket {
    fn drop(&mut self) {
        let _ = fs::unlink(&self.bind_path);
        let _ = fs::unlink(&self.lock_path);
    }
}

/// Builds (bind_path, lock_path) from the given directory and socket name.
fn build_paths(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    (dir.join(name), dir.join(format!("{name}.lock")))
}

/// Attempts to lock the file at the given path.
///
/// If the file does not exist, it will be created.
fn lock_file(path: &Path) -> Result<OwnedFd, SocketError> {
    // Open or create file
    let fd = fs::openat(
        fs::CWD,
        path,
        fs::OFlags::CREATE | fs::OFlags::WRONLY,
        fs::Mode::RUSR | fs::Mode::WUSR,
    )
    .map_err(|err| SocketError::LockOpen(err.into()))?;

    // Lock file
    fs::flock(&fd, fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|err| SocketError::LockAcquire(err.into()))?;

    Ok(fd)
}

/// Returns the `XDG_RUNTIME_DIR` directory.
fn xdg_runtime_dir() -> Result<PathBuf, SocketError> {
    let dir = env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map_err(|_| SocketError::RuntimeDirInvalid)?;

    if !dir.is_absolute() {
        return Err(SocketError::RuntimeDirInvalid);
    }

    Ok(dir)
}
