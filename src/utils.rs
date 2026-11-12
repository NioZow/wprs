// Copyright 2024 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::backtrace::Backtrace;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::panic;
use std::path::Path;
use std::process;
use std::sync::Mutex;
use std::thread::ScopedJoinHandle;

use nix::sys::socket;
use nix::sys::socket::sockopt::PeerCredentials;
use nix::sys::stat;
use nix::sys::stat::Mode;
use nix::unistd;
use smithay::utils::SERIAL_COUNTER;
use smithay::utils::Serial;
use tracing::Level;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::prelude::*;

use crate::prelude::*;

pub fn configure_tracing<P: AsRef<Path>>(
    stderr_log_level: Level,
    path: Option<P>,
    file_log_level: Level,
) -> Result<()> {
    let mut layers = Vec::new();

    let layer = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr.with_max_level(stderr_log_level))
        // TODO(https://github.com/tokio-rs/tracing/pull/2655): uncomment
        // .with_binary_name(true, None)
        // .with_process_id(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE);

    if let Some(path) = path {
        let log_file = File::create(path).location(loc!())?;
        let log_file_writer = Mutex::new(log_file).with_max_level(file_log_level);
        let layer = layer.map_writer(|w| w.and(log_file_writer));
        layers.push(layer.boxed());
    } else {
        layers.push(layer.boxed());
    };

    #[cfg(feature = "tracy")]
    {
        layers
            .push(tracing_tracy::TracyLayer::new(tracing_tracy::DefaultConfig::default()).boxed());
    }

    tracing_subscriber::registry().with(layers).init();
    Ok(())
}

pub fn exit_on_thread_panic() {
    let orig_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let backtrace = Backtrace::capture();
        error!("panic!:\n{panic_info}\n{backtrace}");
        orig_hook(panic_info);
        process::exit(1);
    }));
}

pub fn join_unwrap<T>(handle: ScopedJoinHandle<T>) -> T {
    match handle.join() {
        Ok(t) => t,
        Err(e) => panic::resume_unwind(e),
    }
}

#[derive(Debug)]
pub struct SerialMap {
    map: HashMap<u32, u32>,
    last_serial: u32,
}

impl SerialMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::with_capacity(2000),
            last_serial: 0,
        }
    }

    const PRUNE_THRESHOLD: usize = 2000;
    const PRUNE_AGE: u32 = 1000;

    #[instrument(skip(self), level = "debug")]
    fn prune(&mut self) {
        if self.map.len() > Self::PRUNE_THRESHOLD {
            self.map
                .retain(|&k, _| k > self.last_serial.saturating_sub(Self::PRUNE_AGE));
        }
    }

    pub fn insert(&mut self, client_serial: u32) -> Serial {
        self.last_serial = SERIAL_COUNTER.next_serial().into();
        _ = self.map.insert(self.last_serial, client_serial).is_none();
        self.prune();
        self.last_serial.into()
    }

    pub fn remove(&mut self, server_serial: Serial) -> Option<u32> {
        self.map.remove(&server_serial.into())
    }
}

impl Default for SerialMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Computes the number of chunks that will result from splitting a collection
/// of size len into chunks of chunk_size.
///
/// # Panics
/// If chunk_size = 0.
pub fn n_chunks(len: usize, chunk_size: usize) -> usize {
    assert!(chunk_size != 0);
    if len == 0 {
        0
    } else {
        let n = len / chunk_size;
        let rem = len % chunk_size;
        if rem > 0 { n + 1 } else { n }
    }
}

pub fn bind_user_socket<P: AsRef<Path>>(sock_path: P) -> Result<UnixListener> {
    if sock_path.as_ref().try_exists().location(loc!())? {
        fs::remove_file(&sock_path).location(loc!())?;
    }

    let old_umask = stat::umask(Mode::S_IXUSR | Mode::S_IRWXG | Mode::S_IRWXO);
    let listener = UnixListener::bind(sock_path).location(loc!())?;
    stat::umask(old_umask);

    Ok(listener)
}

/// Creates `dir` (and any missing parents) with mode 0700 if it does not
/// exist, then verifies that it is a real directory (not a symlink) owned by
/// the current effective user and not writable by group or others.
///
/// This guards against another local user pre-creating the socket directory
/// (e.g. /tmp/<user>) and swapping the socket out from under us.
pub fn create_secure_dir<P: AsRef<Path>>(dir: P) -> Result<()> {
    let dir = dir.as_ref();
    // Path::parent() returns "" for a bare relative file name.
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .location(loc!())?;

    let metadata = fs::symlink_metadata(dir).location(loc!())?;
    if !metadata.file_type().is_dir() {
        bail!("Socket directory {dir:?} is not a directory (it may be a symlink).");
    }
    let euid = unistd::geteuid().as_raw();
    if metadata.uid() != euid {
        bail!(
            "Socket directory {dir:?} is owned by uid {}, not the current user (uid {euid}).",
            metadata.uid()
        );
    }
    if metadata.mode() & 0o022 != 0 {
        bail!(
            "Socket directory {dir:?} has insecure permissions {:o}, it must not be writable by group or others.",
            metadata.mode() & 0o7777
        );
    }
    Ok(())
}

/// Verifies that the peer connected to the given unix socket is running as
/// the same user as the current process.
pub fn check_peer_uid<F: AsFd>(stream: &F) -> Result<()> {
    let creds = socket::getsockopt(stream, PeerCredentials).location(loc!())?;
    let euid = unistd::geteuid().as_raw();
    if creds.uid() != euid {
        bail!(
            "Peer on unix socket has uid {} (pid {}), expected uid {euid}.",
            creds.uid(),
            creds.pid()
        );
    }
    Ok(())
}

// https://github.com/nvzqz/static-assertions/issues/21
// https://stackoverflow.com/questions/72582671/const-generics-how-to-ensure-that-usize-const-is-0
pub struct AssertN<const N: usize>;

impl<const N: usize> AssertN<N> {
    pub const NE_0: () = assert!(N != 0);
    pub const MULTIPLE_OF_32: () = assert!(N.is_multiple_of(32));
}

pub struct AssertN3<const N1: usize, const N2: usize, const N3: usize>;

impl<const N1: usize, const N2: usize, const N3: usize> AssertN3<N1, N2, N3> {
    pub const N1_X_N2_EQ_N3: () = assert!(N1.checked_mul(N2).unwrap() == N3);
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    use super::*;

    fn unique_temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("wprs-test-{}-{name}", process::id()))
    }

    #[test]
    fn create_secure_dir_creates_private_dir() {
        let base = unique_temp_path("secure-new");
        let dir = base.join("nested");
        create_secure_dir(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode();
        fs::remove_dir_all(&base).unwrap();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn create_secure_dir_accepts_existing_0755_dir() {
        let dir = unique_temp_path("secure-0755");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let result = create_secure_dir(&dir);
        fs::remove_dir_all(&dir).unwrap();
        result.unwrap();
    }

    #[test]
    fn create_secure_dir_rejects_group_writable_dir() {
        let dir = unique_temp_path("secure-g+w");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o770)).unwrap();
        let result = create_secure_dir(&dir);
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn create_secure_dir_rejects_symlink() {
        let target = unique_temp_path("secure-target");
        let link = unique_temp_path("secure-link");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
        symlink(&target, &link).unwrap();
        let result = create_secure_dir(&link);
        fs::remove_file(&link).unwrap();
        fs::remove_dir_all(&target).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn create_secure_dir_rejects_other_writable_dir() {
        let dir = unique_temp_path("secure-o+w");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o757)).unwrap();
        let result = create_secure_dir(&dir);
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn create_secure_dir_rejects_regular_file() {
        let path = unique_temp_path("secure-file");
        fs::write(&path, b"").unwrap();
        let result = create_secure_dir(&path);
        fs::remove_file(&path).unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn check_peer_uid_accepts_listener_connection() {
        let dir = unique_temp_path("peer-listener");
        create_secure_dir(&dir).unwrap();
        let sock_path = dir.join("test.sock");
        let listener = bind_user_socket(&sock_path).unwrap();
        let client = UnixStream::connect(&sock_path).unwrap();
        let (server, _) = listener.accept().unwrap();
        let server_result = check_peer_uid(&server);
        let client_result = check_peer_uid(&client);
        fs::remove_dir_all(&dir).unwrap();
        server_result.unwrap();
        client_result.unwrap();
    }

    #[test]
    fn check_peer_uid_accepts_same_user() {
        let (a, b) = UnixStream::pair().unwrap();
        check_peer_uid(&a).unwrap();
        check_peer_uid(&b).unwrap();
    }
}
