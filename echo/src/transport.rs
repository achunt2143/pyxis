use anyhow::Result;
use tokio::sync::Mutex;
use std::path::{Path, PathBuf};
use tokio::net::{UnixListener, UnixStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing;
use std::sync::Arc;

pub trait Transport: Send + Sync {
    fn send(&self, data: &[u8]) -> Result<()>;
    fn recv(&self) -> Result<Vec<u8>>;
}

pub struct UnixSocketTransport {
    socket_path: PathBuf,
    listener: Arc<Mutex<Option<UnixListener>>>,
}

impl UnixSocketTransport {
    pub fn new(socket_path: &Path) -> Result<Self> {
        tracing::info!("Initializing Unix Socket Transport: {:?}", socket_path);

        // Create parent directory if needed
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Remove existing socket file if present
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        Ok(UnixSocketTransport {
            socket_path: socket_path.to_path_buf(),
            listener: Arc::new(Mutex::new(None)),
        })
    }

    pub async fn bind(&self) -> Result<()> {
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
        // Clean up socket file
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}