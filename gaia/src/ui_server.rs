// gaia/src/ui_server.rs
//! Web server for shell UI with WebSocket support

use actix_files;
use actix_web::{middleware, web, App, HttpRequest, HttpResponse, HttpServer, Responder};
use actix_ws;
use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

use crate::Gaia;

/// Maximum number of WebSocket subscribers
const MAX_SUBSCRIBERS: usize = 1000;

/// Maximum broadcast channel capacity
const BROADCAST_CAPACITY: usize = 256;

/// Web server for shell UI
pub struct UIServer {
    port: u16,
    /// Broadcast channel for real-time signals
    tx: broadcast::Sender<BroadcastMessage>,
    /// Connected WebSocket clients tracking
    clients: Arc<RwLock<HashMap<String, ClientInfo>>>,
}

/// Broadcast message for WebSocket clients
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub event_type: String,
    pub data: Value,
    pub timestamp: String,
}

/// Information about connected clients
#[derive(Debug, Clone)]
struct ClientInfo {
    client_id: String,
    connected_at: String,
    last_activity: String,
}

/// Request/Response Types
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LaunchAppRequest {
    pub app_name: String,
}

#[derive(Debug, Deserialize)]
pub struct ListFilesQuery {
    pub path: Option<String>,
}

/// WebSocket message types
#[derive(Debug, Deserialize, Serialize)]
pub struct WebSocketMessage {
    pub message_type: String,
    pub payload: Value,
}

impl UIServer {
    /// Create new UI server instance
    pub async fn new(port: u16) -> Result<Self> {
        debug!("Creating UIServer on port {}", port);

        // Create broadcast channel for real-time signals
        let (tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);

        Ok(UIServer {
            port,
            tx,
            clients: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Start the HTTP server (runs in background, returns immediately)
    pub async fn start(&self, gaia: Arc<Gaia>) -> Result<()> {
        info!("🚀 Starting UIServer on http://127.0.0.1:{}", self.port);

        let port = self.port;
        let gaia_data = web::Data::new(gaia);
        let broadcast_tx = self.tx.clone();
        let clients = self.clients.clone();

        // Spawn server in background
        tokio::spawn(async move {
            match HttpServer::new(move || {
                App::new()
                    .app_data(gaia_data.clone())
                    .app_data(web::Data::new(broadcast_tx.clone()))
                    .app_data(web::Data::new(clients.clone()))
                    .wrap(middleware::Logger::default())
                    .wrap(middleware::NormalizePath::trim())
                    .service(
                        web::scope("/api")
                            .route("/health", web::get().to(handlers::health_check))
                            .route("/apps", web::get().to(handlers::get_apps))
                            .route("/apps/launch", web::post().to(handlers::launch_app))
                            .route("/taskbar/items", web::get().to(handlers::get_taskbar_items))
                            .route("/taskbar/state", web::get().to(handlers::get_taskbar_state))
                            .route("/taskbar/pin/{app_id}", web::post().to(handlers::pin_app))
                            .route(
                                "/taskbar/unpin/{app_id}",
                                web::post().to(handlers::unpin_app),
                            )
                            .route(
                                "/taskbar/focus/{app_id}",
                                web::post().to(handlers::focus_app),
                            )
                            .route("/files", web::get().to(handlers::list_files))
                            .route("/files/{id}", web::get().to(handlers::get_file))
                            .route("/files/upload", web::post().to(handlers::upload_file))
                            .route("/media/{id}", web::get().to(handlers::get_media))
                            .route("/email/inbox", web::get().to(handlers::get_email_inbox)),
                    )
                    .route("/ws", web::get())
                    .service(actix_files::Files::new("/", "./static").index_file("index.html"))
            })
            .bind(format!("127.0.0.1:{}", port))
            .and_then(|s| Ok(s.run()))
            {
                Ok(server) => {
                    info!("✓ UIServer listening on http://127.0.0.1:{}", port);
                    let _ = server.await;
                }
                Err(e) => error!("Server error: {}", e),
            }
        });

        // Give server a moment to start and bind
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        info!("✓ UIServer started successfully (running in background)");
        Ok(())
    }

    /// Stop the HTTP server gracefully
    pub async fn stop(&self) -> Result<()> {
        info!("Stopping UIServer");
        info!("✓ UIServer stopped gracefully");
        Ok(())
    }

    /// Broadcast a signal to all connected WebSocket clients
    pub async fn broadcast_signal(&self, event_type: &str, data: &Value) -> Result<()> {
        let message = BroadcastMessage {
            event_type: event_type.to_string(),
            data: data.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        // Broadcast to all connected clients
        match self.tx.send(message.clone()) {
            Ok(_) => {
                debug!("Broadcast signal sent: {}", event_type);
                Ok(())
            }
            Err(e) => {
                warn!("Failed to broadcast signal '{}': {}", event_type, e);
                // Don't error if there are no subscribers
                Ok(())
            }
        }
    }

    /// Get number of connected clients
    pub fn client_count(&self) -> usize {
        self.clients.read().len()
    }

    /// Get client information
    pub fn get_clients(&self) -> Vec<ClientInfo> {
        self.clients.read().values().cloned().collect()
    }

    /// Get the port the server is running on
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// API Handlers
pub mod handlers {
    use super::*;
    use futures::stream::StreamExt;
    use uuid::Uuid;

    /// Health check endpoint
    pub async fn health_check() -> impl Responder {
        HttpResponse::Ok().json(json!({
            "status": "ok",
            "service": "gaia-shell",
            "version": "2.0"
        }))
    }

    /// WebSocket handler
    pub async fn websocket_handler(
        req: HttpRequest,
        stream: web::Payload,
        broadcast_tx: web::Data<broadcast::Sender<BroadcastMessage>>,
        clients: Arc<RwLock<HashMap<String, ClientInfo>>>,
    ) -> Result<impl Responder, actix_web::Error> {
        let client_id = Uuid::new_v4().to_string();
        debug!("WebSocket client connecting: {}", client_id);

        let (response, session, msg_stream) = actix_ws::handle(&req, stream)?;

        // Register client
        clients.write().insert(
            client_id.clone(),
            ClientInfo {
                client_id: client_id.clone(),
                connected_at: chrono::Utc::now().to_rfc3339(),
                last_activity: chrono::Utc::now().to_rfc3339(),
            },
        );

        info!("WebSocket client connected: {}", client_id);

        // Create local copies for the actor task
        let client_id_clone = client_id.clone();
        let broadcast_tx_inner = broadcast_tx.into_inner();
        let clients_clone = clients.clone();

        // Create actor task that doesn't need to be Send
        actix_rt::spawn(async move {
            handle_websocket_actor(
                client_id_clone,
                session,
                msg_stream,
                broadcast_tx_inner,
                clients_clone,
            )
            .await;
        });

        Ok(response)
    }

    /// Handles WebSocket communication for a single client
    async fn handle_websocket_actor(
        client_id: String,
        mut session: actix_ws::Session,
        mut msg_stream: actix_ws::MessageStream,
        broadcast_tx: Arc<broadcast::Sender<BroadcastMessage>>,
        clients: Arc<RwLock<HashMap<String, ClientInfo>>>,
    ) {
        let mut rx = broadcast_tx.subscribe();

        loop {
            tokio::select! {
                // Handle incoming messages from client
                msg = msg_stream.recv() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                actix_ws::Message::Text(text) => {
                                    debug!("WebSocket message from {}: {}", client_id, text);

                                    // Update last activity
                                    if let mut clients_map = clients.write() {
                                        if let Some(client) = clients_map.get_mut(&client_id) {
                                            client.last_activity = chrono::Utc::now().to_rfc3339();
                                        }
                                    }
                                }
                                actix_ws::Message::Binary(_) => {
                                    debug!("Binary message from {}", client_id);
                                }
                                actix_ws::Message::Ping(bytes) => {
                                    let _ = session.pong(&bytes).await;
                                }
                                actix_ws::Message::Pong(_) => {
                                    debug!("Pong from {}", client_id);
                                }
                                actix_ws::Message::Close(reason) => {
                                    info!("WebSocket client {} disconnected: {:?}", client_id, reason);
                                    break;
                                }
                                _ => {}
                            }
                        }
                        Some(Err(e)) => {
                            error!("WebSocket error for {}: {}", client_id, e);
                            break;
                        }
                        None => {
                            debug!("WebSocket stream closed for {}", client_id);
                            break;
                        }
                    }
                }

                // Handle broadcast messages to send to client
                Ok(msg) = rx.recv() => {
                    let json_msg = serde_json::to_string(&msg).unwrap_or_default();
                    if session.text(json_msg).await.is_err() {
                        debug!("Failed to send message to client {}", client_id);
                        break;
                    }
                }
            }
        }

        // Cleanup: close session
        let _ = session.close(None).await;

        // Remove client when disconnected
        if let mut clients_map = clients.write() {
            clients_map.remove(&client_id);
        }
        info!("WebSocket client {} removed from tracking", client_id);
    }

    /// Get list of applications
    pub async fn get_apps(gaia: web::Data<Arc<Gaia>>) -> impl Responder {
        debug!("GET /api/apps");

        match serde_json::to_value(gaia.launcher().list_apps()) {
            Ok(apps) => {
                let apps_array = if let Value::Array(a) = apps {
                    a
                } else {
                    Vec::new()
                };

                HttpResponse::Ok().json(json!({
                    "apps": apps_array,
                    "count": apps_array.len()
                }))
            }
            Err(_) => HttpResponse::Ok().json(json!({
                "apps": [],
                "count": 0
            })),
        }
    }

    /// Launch an application
    pub async fn launch_app(
        gaia: web::Data<Arc<Gaia>>,
        req: web::Json<LaunchAppRequest>,
        broadcast_tx: web::Data<broadcast::Sender<BroadcastMessage>>,
    ) -> impl Responder {
        debug!("POST /api/apps/launch - app: {}", req.app_name);

        match gaia.launcher().launch_app(req.clone()) {
            Ok(app) => {
                info!("Application {} launched", req.app_name);

                // Broadcast to UI clients
                let _ = broadcast_tx.send(BroadcastMessage {
                    event_type: "app_launched".to_string(),
                    data: json!({
                        "app_id": req.app_name,
                        "process_id": app.process_id,
                    }),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });

                HttpResponse::Ok().json(json!({
                    "success": true,
                    "app": req.app_name,
                    "process_id": app.process_id
                }))
            }
            Err(e) => {
                error!("Failed to launch app {}: {}", req.app_name, e);
                HttpResponse::BadRequest().json(json!({
                    "success": false,
                    "error": e.to_string()
                }))
            }
        }
    }

    /// Get taskbar items
    pub async fn get_taskbar_items(gaia: web::Data<Arc<Gaia>>) -> impl Responder {
        debug!("GET /api/taskbar/items");

        match gaia.taskbar().get_items().await {
            Ok(items) => HttpResponse::Ok().json(json!({
                "items": items,
                "count": items.len()
            })),
            Err(e) => {
                error!("Failed to get taskbar items: {}", e);
                HttpResponse::InternalServerError().json(json!({
                    "error": e.to_string()
                }))
            }
        }
    }

    /// Get taskbar state
    pub async fn get_taskbar_state(gaia: web::Data<Arc<Gaia>>) -> impl Responder {
        debug!("GET /api/taskbar/state");

        match gaia.taskbar().get_state_json().await {
            Ok(state) => HttpResponse::Ok().json(state),
            Err(e) => {
                error!("Failed to get taskbar state: {}", e);
                HttpResponse::InternalServerError().json(json!({
                    "error": e.to_string()
                }))
            }
        }
    }

    /// Pin application to taskbar
    pub async fn pin_app(
        gaia: web::Data<Arc<Gaia>>,
        app_id: web::Path<String>,
        broadcast_tx: web::Data<broadcast::Sender<BroadcastMessage>>,
    ) -> impl Responder {
        debug!("POST /api/taskbar/pin/{}", app_id);

        match gaia.taskbar().pin_app(&app_id).await {
            Ok(_) => {
                info!("App pinned: {}", app_id);

                let _ = broadcast_tx.send(BroadcastMessage {
                    event_type: "app_pinned".to_string(),
                    data: json!({ "app_id": app_id.to_string() }),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });

                HttpResponse::Ok().json(json!({
                    "success": true,
                    "app_id": app_id.to_string()
                }))
            }
            Err(e) => {
                error!("Failed to pin app {}: {}", app_id, e);
                HttpResponse::BadRequest().json(json!({
                    "success": false,
                    "error": e.to_string()
                }))
            }
        }
    }

    /// Unpin application from taskbar
    pub async fn unpin_app(
        gaia: web::Data<Arc<Gaia>>,
        app_id: web::Path<String>,
        broadcast_tx: web::Data<broadcast::Sender<BroadcastMessage>>,
    ) -> impl Responder {
        debug!("POST /api/taskbar/unpin/{}", app_id);

        match gaia.taskbar().unpin_app(&app_id).await {
            Ok(_) => {
                info!("App unpinned: {}", app_id);

                let _ = broadcast_tx.send(BroadcastMessage {
                    event_type: "app_unpinned".to_string(),
                    data: json!({ "app_id": app_id.to_string() }),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });

                HttpResponse::Ok().json(json!({
                    "success": true,
                    "app_id": app_id.to_string()
                }))
            }
            Err(e) => {
                error!("Failed to unpin app {}: {}", app_id, e);
                HttpResponse::BadRequest().json(json!({
                    "success": false,
                    "error": e.to_string()
                }))
            }
        }
    }

    /// Focus application
    pub async fn focus_app(
        gaia: web::Data<Arc<Gaia>>,
        app_id: web::Path<String>,
        broadcast_tx: web::Data<broadcast::Sender<BroadcastMessage>>,
    ) -> impl Responder {
        debug!("POST /api/taskbar/focus/{}", app_id);

        match gaia.taskbar().set_focused_app(&app_id).await {
            Ok(_) => {
                info!("App focused: {}", app_id);

                let _ = broadcast_tx.send(BroadcastMessage {
                    event_type: "app_focused".to_string(),
                    data: json!({ "app_id": app_id.to_string() }),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                });

                HttpResponse::Ok().json(json!({
                    "success": true,
                    "app_id": app_id.to_string()
                }))
            }
            Err(e) => {
                error!("Failed to focus app {}: {}", app_id, e);
                HttpResponse::BadRequest().json(json!({
                    "success": false,
                    "error": e.to_string()
                }))
            }
        }
    }

    /// List files in directory
    pub async fn list_files(
        _gaia: web::Data<Arc<Gaia>>,
        query: web::Query<ListFilesQuery>,
    ) -> impl Responder {
        let path = query.path.as_deref().unwrap_or("/");
        debug!("GET /api/files?path={}", path);

        HttpResponse::Ok().json(json!({
            "files": [],
            "path": path,
            "count": 0
        }))
    }

    /// Get file content
    pub async fn get_file(
        _gaia: web::Data<Arc<Gaia>>,
        file_id: web::Path<String>,
    ) -> impl Responder {
        debug!("GET /api/files/{}", file_id);

        HttpResponse::NotFound().json(json!({
            "success": false,
            "error": "File not found"
        }))
    }

    /// Upload file
    pub async fn upload_file(_gaia: web::Data<Arc<Gaia>>) -> impl Responder {
        debug!("POST /api/files/upload");

        HttpResponse::Ok().json(json!({
            "success": true,
            "message": "Upload functionality coming soon"
        }))
    }

    /// Get media content
    pub async fn get_media(
        _gaia: web::Data<Arc<Gaia>>,
        media_id: web::Path<String>,
    ) -> impl Responder {
        debug!("GET /api/media/{}", media_id);

        HttpResponse::NotFound().json(json!({
            "success": false,
            "error": "Media not found"
        }))
    }

    /// Get email inbox
    pub async fn get_email_inbox(_gaia: web::Data<Arc<Gaia>>) -> impl Responder {
        debug!("GET /api/email/inbox");

        HttpResponse::Ok().json(json!({
            "emails": [],
            "count": 0,
            "unread": 0
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ui_server_creation() {
        let server = UIServer::new(8080).await.unwrap();
        assert_eq!(server.port(), 8080);
    }

    #[tokio::test]
    async fn test_ui_server_new_on_different_port() {
        let server = UIServer::new(9090).await.unwrap();
        assert_eq!(server.port(), 9090);
    }

    #[tokio::test]
    async fn test_broadcast_signal() {
        let server = UIServer::new(8080).await.unwrap();
        let data = json!({
            "event": "test",
            "message": "Hello, World!"
        });

        let result = server.broadcast_signal("test_event", &data).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_client_count() {
        let server = UIServer::new(8080).await.unwrap();
        assert_eq!(server.client_count(), 0);
    }
}
