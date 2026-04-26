use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// ============================================================================
// FIREFOX NATIVE MESSAGING PROTOCOL STRUCTURES
// ============================================================================

#[derive(Debug, Deserialize)]
struct NativeRequest {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(default)]
    request_id: Option<Value>,
    #[serde(default)]
    service: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct NativeResponse {
    #[serde(rename = "type")]
    msg_type: String,
    request_id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ============================================================================
// ECHO IPC MESSAGE STRUCTURES
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EchoMessage {
    pub header: EchoMessageHeader,
    pub body: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EchoMessageHeader {
    pub message_id: String,
    #[serde(rename = "message_type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender: Option<String>,
    pub destination: String,
    pub interface: String,
    pub method: String,
    #[serde(default)]
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    pub timestamp: u64,
}

impl EchoMessage {
    fn method_call(destination: String, interface: String, method: String, body: Value) -> Self {
        EchoMessage {
            header: EchoMessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: "METHOD_CALL".to_string(),
                sender: None,
                destination,
                interface,
                method,
                signature: "".to_string(),
                reply_to: None,
                timestamp: now_ms(),
            },
            body,
        }
    }

    fn handshake() -> Self {
        EchoMessage {
            header: EchoMessageHeader {
                message_id: Uuid::new_v4().to_string(),
                message_type: "HANDSHAKE".to_string(),
                sender: None,
                destination: "org.geckoOS.Echo".to_string(),
                interface: "org.geckoOS.Echo".to_string(),
                method: "HANDSHAKE".to_string(),
                signature: "".to_string(),
                reply_to: None,
                timestamp: now_ms(),
            },
            body: json!({
                "version": "1.0.0",
                "client": "geckoOS-native-host"
            }),
        }
    }
}

// ============================================================================
// ECHO IPC BROKER
// ============================================================================

struct EchoBroker {
    tx: mpsc::Sender<EchoBrokerRequest>,
}

struct EchoBrokerRequest {
    message: EchoMessage,
    reply: tokio::sync::oneshot::Sender<Result<Value>>,
}

// ============================================================================
// MAIN
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::DEBUG)
        .init();

    info!("GeckoOS Native Host starting");

    let socket_path = echo_socket_path();
    info!("Echo socket path: {}", socket_path.display());

    let (tx, rx) = mpsc::channel::<EchoBrokerRequest>(32);
    let broker = Arc::new(EchoBroker { tx });

    tokio::spawn(echo_broker_loop(rx, socket_path));

    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    if let Err(e) = native_message_loop(&broker, &mut stdin, &mut stdout).await {
        error!("Fatal native host error: {}", e);
        return Err(e);
    }

    Ok(())
}

// ============================================================================
// NATIVE <-> HOST LOOP
// ============================================================================

async fn native_message_loop(
    broker: &Arc<EchoBroker>,
    stdin: &mut impl Read,
    stdout: &mut impl Write,
) -> Result<()> {
    loop {
        let msg = match read_native_message(stdin) {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                info!("Firefox stdin EOF, shutting down native host");
                break;
            }
            Err(e) => {
                error!("Failed to read native message from Firefox: {}", e);
                break; // or continue; but break will close gracefully
            }
        };

        info!("Received native message type: {}", msg.msg_type);

        let response = handle_native_message(broker, msg).await;

        write_native_message(stdout, response)?;
        tokio::task::yield_now().await;
    }

    Ok(())
}

async fn handle_native_message(broker: &Arc<EchoBroker>, msg: NativeRequest) -> NativeResponse {
    match msg.msg_type.as_str() {
        "PING" => {
            debug!("Received PING");
            NativeResponse {
                msg_type: "PONG".to_string(),
                request_id: msg.request_id.unwrap_or(json!(null)),
                result: Some(json!({ "status": "ok" })),
                error: None,
            }
        }
        "HANDSHAKE" => {
            debug!("Received HANDSHAKE from browser");
            NativeResponse {
                msg_type: "HANDSHAKEACK".to_string(),
                request_id: msg.request_id.unwrap_or(json!(null)),
                result: Some(json!({
                    "version": "1.0.0",
                    "status": "ready"
                })),
                error: None,
            }
        }
        "ECHOSERVICECALL" => {
            debug!("Received ECHOSERVICECALL");

            let request_id = match msg.request_id {
                Some(id) => id,
                None => return error_response(json!(null), "Missing requestId"),
            };

            let service = match msg.service {
                Some(s) => s,
                None => return error_response(request_id, "Missing service"),
            };

            let method = match msg.method {
                Some(m) => m,
                None => return error_response(request_id, "Missing method"),
            };

            let params = msg.params.unwrap_or(json!({}));

            call_echo_service(broker, request_id, service, method, params).await
        }
        _ => {
            warn!("Unknown native message type: {}", msg.msg_type);
            error_response(json!(null), "Unknown message type")
        }
    }
}

// ============================================================================
// ECHO CALL ROUTING
// ============================================================================

async fn call_echo_service(
    broker: &Arc<EchoBroker>,
    request_id: Value,
    service: String,
    method: String,
    params: Value,
) -> NativeResponse {
    let destination = format!("org.geckoOS.{}", &service);
    let method = method;

    let echo_msg = EchoMessage::method_call(destination.clone(), destination, method, params);

    debug!(
        "Calling Echo service: {} (requestId: {:?})",
        echo_msg.header.destination, request_id
    );

    let (tx, rx) = tokio::sync::oneshot::channel::<Result<Value>>();

    if let Err(e) = broker
        .tx
        .send(EchoBrokerRequest {
            message: echo_msg,
            reply: tx,
        })
        .await
    {
        error!("Echo broker channel error: {}", e);
        return error_response(request_id, "Echo broker unavailable");
    }

    match tokio::time::timeout(std::time::Duration::from_secs(25), rx).await {
        Ok(Ok(Ok(result))) => success_response(request_id, result),
        Ok(Ok(Err(e))) => {
            error!("Echo error: {}", e);
            error_response(request_id, &format!("Echo error: {}", e))
        }
        Ok(Err(_)) => {
            error!("Echo broker dropped");
            error_response(request_id, "Echo broker dropped")
        }
        Err(_) => {
            error!("Echo call timed out");
            error_response(request_id, "Echo call timed out")
        }
    }
}

// ============================================================================
// ECHO BROKER LOOP
// ============================================================================

async fn echo_broker_loop(mut rx: mpsc::Receiver<EchoBrokerRequest>, socket_path: PathBuf) {
    use std::collections::HashMap;

    loop {
        match UnixStream::connect(&socket_path).await {
            Ok(stream) => {
                info!("Connected to Echo daemon");
                let (mut read_half, mut write_half) = stream.into_split();

                // Handshake
                if let Err(e) = send_echo_message(&mut write_half, &EchoMessage::handshake()).await
                {
                    error!("Failed to send handshake: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    continue;
                }

                match recv_echo_message(&mut read_half).await {
                    Ok(resp) => {
                        info!(
                            "Handshake response received from Echo: {}",
                            resp.header.message_type
                        );
                    }
                    Err(e) => {
                        error!("Failed to receive handshake response: {}", e);
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        continue;
                    }
                }

                info!("Echo broker ready to process requests");

                // Pending map: original message_id -> oneshot sender
                let mut pending: HashMap<String, tokio::sync::oneshot::Sender<Result<Value>>> =
                    HashMap::new();

                loop {
                    tokio::select! {
                        // Outbound requests from native host
                        maybe_req = rx.recv() => {
                            match maybe_req {
                                Some(req) => {
                                    let msg_id = req.message.header.message_id.clone();
                                    debug!("Processing request to Echo (msg_id={})", msg_id);
                                    // Store pending before sending
                                    pending.insert(msg_id.clone(), req.reply);

                                    if let Err(e) = send_echo_message(&mut write_half, &req.message).await {
                                        error!("Echo send error: {}", e);
                                        if let Some(reply) = pending.remove(&msg_id) {
                                            let _ = reply.send(Err(anyhow!(e)));
                                        }
                                        break; // reconnect
                                    }
                                }
                                None => {
                                    warn!("Echo broker channel closed; exiting");
                                    return;
                                }
                            }
                        }

                        // Incoming messages from Echo daemon (replies or signals)
                        incoming = recv_echo_message(&mut read_half) => {
                            match incoming {
                                Ok(msg) => {
                                    debug!("Received Echo message: type={}", msg.header.message_type);
                                    match msg.header.message_type.as_str() {
                                        "METHOD_RETURN" | "ERROR" => {
                                            // Prefer reply_to as correlation key if set, otherwise message_id
                                            let key = msg.header.reply_to
                                                .clone()
                                                .unwrap_or_else(|| msg.header.message_id.clone());

                                            if let Some(reply) = pending.remove(&key) {
                                                let result = if msg.header.message_type == "METHOD_RETURN" {
                                                    Ok(msg.body)
                                                } else {
                                                    // You use "Error" vs "ERROR" inconsistently; match daemon/Evo side
                                                    let err_text = msg
                                                        .body
                                                        .get("error")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("Echo error")
                                                        .to_string();
                                                    Err(anyhow!(err_text))
                                                };
                                                let _ = reply.send(result);
                                            } else {
                                                debug!("No pending Echo request for key={}", key);
                                            }
                                        }
                                        _ => {
                                            // Signals or other message types can be logged for now
                                            debug!("Ignoring unsolicited Echo message type={}", msg.header.message_type);
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("Echo receive error: {}", e);
                                    // Fail all pending requests on connection loss
                                    for (_, reply) in pending.drain() {
                                        let _ = reply.send(Err(anyhow!("Echo connection lost: {}", e)));
                                    }
                                    break; // reconnect
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect to Echo: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        }
    }
}

// async fn process_echo_request(stream: UnixStream, req: EchoBrokerRequest) -> Result<()> {
//     let (mut read_half, mut write_half) = stream.into_split();
//     send_echo_message(&mut write_half, &req.message).await?;
//     let response = recv_echo_message(&mut read_half).await?;

//     let result = if response.header.message_type == "METHOD_RETURN" {
//         Ok(response.body)
//     } else if response.header.message_type == "Error" {
//         let msg = response
//             .body
//             .get("message")
//             .and_then(|v| v.as_str())
//             .unwrap_or("Unknown error")
//             .to_string();
//         Err(anyhow!(msg))
//     } else {
//         Err(anyhow!(
//             "Unexpected Echo message type: {}",
//             response.header.message_type
//         ))
//     };

//     let _ = req.reply.send(result);
//     Ok(())
// }

// ============================================================================
// ECHO MESSAGE I/O
// ============================================================================

// async fn send_echo_message(stream: &mut UnixStream, msg: &EchoMessage) -> Result<()> {
//     let data = serde_json::to_vec(msg)?;
//     let len = data.len() as u32;

//     stream.write_u32_le(len).await?;
//     stream.write_all(&data).await?;
//     stream.flush().await?;

//     debug!("Sent Echo message: {} bytes", data.len());
//     Ok(())
// }
async fn send_echo_message(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    msg: &EchoMessage,
) -> Result<()> {
    let data = serde_json::to_vec(msg)?;
    let len = data.len() as u32;
    write.write_u32_le(len).await?;
    write.write_all(&data).await?;
    write.flush().await?;
    debug!("Sent Echo message: {} bytes", data.len());
    Ok(())
}

async fn recv_echo_message(read: &mut tokio::net::unix::OwnedReadHalf) -> Result<EchoMessage> {
    let len = read.read_u32_le().await? as usize;
    if len > 1024 * 1024 {
        return Err(anyhow!("Message too large: {} bytes", len));
    }

    let mut buffer = vec![0u8; len];
    read.read_exact(&mut buffer).await?;
    debug!("Received Echo message: {} bytes", len);

    let msg: EchoMessage = serde_json::from_slice(&buffer)?;
    Ok(msg)
}

// async fn recv_echo_message(stream: &mut UnixStream) -> Result<EchoMessage> {
//     let len = stream.read_u32_le().await? as usize;

//     if len > 1024 * 1024 {
//         return Err(anyhow!("Message too large: {} bytes", len));
//     }

//     let mut buffer = vec![0u8; len];
//     stream.read_exact(&mut buffer).await?;
//     debug!("Received Echo message: {} bytes", len);

//     let msg: EchoMessage = serde_json::from_slice(&buffer)?;
//     Ok(msg)
// }

// ============================================================================
// HELPERS / NATIVE I/O
// ============================================================================

fn echo_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("geckoOS/echo.sock")
    } else {
        PathBuf::from("/tmp/geckoOS/echo.sock")
    }
}

fn success_response(request_id: Value, result: Value) -> NativeResponse {
    NativeResponse {
        msg_type: "SUCCESS".to_string(),
        request_id,
        result: Some(result),
        error: None,
    }
}

fn error_response(request_id: Value, error: &str) -> NativeResponse {
    NativeResponse {
        msg_type: "ERROR".to_string(),
        request_id,
        result: None,
        error: Some(error.to_string()),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn read_native_message(stdin: &mut impl Read) -> Result<Option<NativeRequest>> {
    let mut len_buf = [0u8; 4];
    match stdin.read_exact(&mut len_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            // Firefox closed the pipe
            return Ok(None);
        }
        Err(e) => {
            return Err(anyhow!("Failed to read length prefix from stdin: {}", e));
        }
    }

    let len = u32::from_le_bytes(len_buf) as usize;

    if len == 0 {
        return Err(anyhow!("Got zero-length native message"));
    }

    if len > 1024 * 1024 {
        return Err(anyhow!("Message too large: {} bytes", len));
    }

    let mut buffer = vec![0u8; len];
    if let Err(e) = stdin.read_exact(&mut buffer) {
        return Err(anyhow!("Failed to read message body from stdin: {}", e));
    }

    // Log raw JSON for debugging
    if let Ok(text) = std::str::from_utf8(&buffer) {
        debug!("Raw native message JSON: {}", text);
    } else {
        debug!("Raw native message (non-UTF8, {} bytes)", len);
    }

    let msg: NativeRequest = serde_json::from_slice(&buffer)
        .map_err(|e| anyhow!("Failed to decode NativeRequest JSON: {}", e))?;

    Ok(Some(msg))
}

fn write_native_message(stdout: &mut impl Write, msg: NativeResponse) -> Result<()> {
    let data = serde_json::to_vec(&msg)?;
    let len = data.len() as u32;

    stdout
        .write_all(&len.to_le_bytes())
        .map_err(|e| anyhow!("Failed to write length prefix to stdout: {}", e))?;
    stdout
        .write_all(&data)
        .map_err(|e| anyhow!("Failed to write message body to stdout: {}", e))?;
    stdout
        .flush()
        .map_err(|e| anyhow!("Failed to flush stdout: {}", e))?;

    debug!("Sent native response: {} bytes", data.len());
    Ok(())
}
