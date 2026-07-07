use anyhow::Result;
use tokio::sync::Mutex;
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub trait Transport: Send + Sync {
    fn send(&self, data: &[u8]) -> Result<()>;
    fn recv(&self) -> Result<Vec<u8>>;
}

pub struct UnixSocketTransport {
    socket_path: PathBuf,
    listener: Arc<Mutex<Option<UnixListener>>>,
    // Set true only by `bind()` on success. Distinguishes a server-side
    // transport (the one that actually owns the listening socket) from a
    // client-side one (`EchoClient::connect_to` builds a transport just to
    // call `.connect()`, then drops it immediately) — see `Drop`'s own
    // comment for why this matters. Plain atomic rather than under the
    // `listener` mutex since `Drop::drop` can't `.await` a lock.
    is_bound: AtomicBool,
}

impl UnixSocketTransport {
    pub fn new(socket_path: &Path) -> Result<Self> {
        tracing::info!("Initializing Unix Socket Transport: {:?}", socket_path);

        // Create parent directory if needed
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        Ok(UnixSocketTransport {
            socket_path: socket_path.to_path_buf(),
            listener: Arc::new(Mutex::new(None)),
            is_bound: AtomicBool::new(false),
        })
    }

    pub async fn bind(&self) -> Result<()> {
        // Stale-socket cleanup belongs HERE, not in `new()`: `new()` is
        // called by every CLIENT connection too (`EchoClient::connect_to`
        // constructs a `UnixSocketTransport` the same way the daemon does),
        // and unlinking the path there meant every single client call
        // (e.g. `echo-cli`) deleted the running daemon's live socket file
        // an instant before trying to connect to it — a real bug found via
        // an actual boot ("echo-cli: No such file or directory" despite
        // `ps`/the daemon's own log showing it healthy and already serving
        // Evo's connection, which works because evo's echo_ipc.rs uses its
        // own hand-rolled UnixStream::connect() and never goes through
        // this shared client path at all). Only the actual bind side
        // should ever remove a stale path left over from a previous
        // unclean daemon shutdown.
        if self.socket_path.exists() {
            std::fs::remove_file(&self.socket_path)?;
        }

        tracing::info!("Binding to: {:?}", self.socket_path);
        let listener = UnixListener::bind(&self.socket_path)?;

        // Set permissions to allow all users (0666)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let permissions = std::fs::Permissions::from_mode(0o666);
            std::fs::set_permissions(&self.socket_path, permissions)?;
        }

        let mut listener_guard = self.listener.lock().await;
        *listener_guard = Some(listener);
        drop(listener_guard);
        self.is_bound.store(true, Ordering::Release);

        tracing::info!("Listening on: {:?}", self.socket_path);
        Ok(())
    }

    pub async fn accept(&self) -> Result<UnixStream> {
        let listener = self.listener.lock().await;
        if let Some(ref listener) = *listener {
            let stream = listener.accept().await?;
            tracing::trace!("Accepted new connection");
            Ok(stream.0)
        } else {
            Err(anyhow::anyhow!("Transport not bound"))
        }
    }

    pub async fn connect(&self) -> Result<UnixStream> {
        tracing::debug!("Connecting to: {:?}", self.socket_path);
        let stream = UnixStream::connect(&self.socket_path).await?;
        tracing::debug!("Connected to Echo daemon");
        Ok(stream)
    }

    /// Send a message with length prefix (4 bytes, LITTLE-ENDIAN - CRITICAL!)
    /// MUST MATCH: native-host and daemon must both use little-endian
    pub async fn send_message(stream: &mut UnixStream, data: &[u8]) -> Result<()> {
        // Send length prefix (4 bytes, little-endian - u32)
        let len = data.len() as u32;
        stream.write_u32_le(len).await?;

        // Send message data
        stream.write_all(data).await?;
        stream.flush().await?;

        tracing::trace!("Sent {} bytes (total with length prefix: {} bytes)", data.len(), data.len() + 4);
        Ok(())
    }

    /// Receive a message with length prefix (4 bytes, LITTLE-ENDIAN - CRITICAL!)
    pub async fn recv_message(stream: &mut UnixStream) -> Result<Vec<u8>> {
        // Read length prefix (4 bytes, little-endian)
        let len = stream.read_u32_le().await? as usize;

        // Sanity check
        if len > 16 * 1024 * 1024 {
            // Max 16MB
            return Err(anyhow::anyhow!("Message too large: {} bytes", len));
        }

        // Read message data
        let mut buffer = vec![0u8; len];
        stream.read_exact(&mut buffer).await?;

        tracing::trace!("Received {} bytes (total with length prefix: {} bytes)", len, len + 4);
        Ok(buffer)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for UnixSocketTransport {
    fn drop(&mut self) {
        // Only the transport that actually bound the listener should ever
        // remove the socket file. Without this check, every CLIENT
        // transport (e.g. `EchoClient::connect_to`'s local `transport`
        // variable, dropped the instant `connect()` returns) would delete
        // the daemon's live socket file on every single client
        // disconnect — a second, independent instance of the exact same
        // bug `bind()`'s own comment describes: found via a real boot
        // where `echo-cli` failed with ENOENT even after that first fix,
        // because the PREVIOUS client's transport had already deleted the
        // path on its own way out.
        if self.is_bound.load(Ordering::Acquire) && self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduces the exact reported bug: a daemon-side transport binds
    /// and is actively serving one client; a second client then connects
    /// fresh (as `echo-cli` does on every invocation). Before the fix,
    /// both the first client's `new()` (deleting the file up front) and
    /// its `Drop` (deleting the file again on disconnect) would leave the
    /// second connection attempt failing with ENOENT even though the
    /// daemon was healthy and already serving the first client.
    #[tokio::test]
    async fn client_connect_does_not_break_daemon_socket_for_next_client() {
        let dir = std::env::temp_dir().join(format!("echo-transport-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("echo.sock");

        // Server side: bind and keep alive for the whole test.
        let server = UnixSocketTransport::new(&socket_path).unwrap();
        server.bind().await.unwrap();

        // First client connects (like echo-daemon's own long-lived
        // connections, or a prior echo-cli invocation) and stays alive.
        let client1 = UnixSocketTransport::new(&socket_path).unwrap();
        let _stream1 = client1.connect().await.unwrap();
        let _accepted1 = server.accept().await.unwrap();

        // Second client (e.g. a fresh `echo-cli` call) must still be able
        // to connect — this is exactly what broke before the fix.
        let client2 = UnixSocketTransport::new(&socket_path).unwrap();
        let connect2 = client2.connect().await;
        assert!(
            connect2.is_ok(),
            "second client failed to connect: {:?}",
            connect2.err()
        );

        // Dropping client transports must not remove the daemon's socket
        // file out from under it.
        drop(client1);
        drop(client2);
        assert!(
            socket_path.exists(),
            "socket file was removed by a client-side transport's Drop"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}