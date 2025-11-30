// ============================================================================
// UI API MODULE
// File: gaia/src/ui/api.rs
// Purpose: RESTful API endpoints
// ============================================================================

use actix_web::{get, post, web, HttpResponse, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::managers::AppManager;
use crate::services::ServiceRegistry;
use crate::cache_manager::CacheManager;
use crate::app_loader::AppLoader;
use crate::file_service::{FileService, FileInfo};
use crate::media_service::{MediaService, MediaFile};
use crate::email_service::{EmailService, Email};

pub struct AppState {
    pub app_manager: Arc<RwLock<AppManager>>,
    pub service_registry: Arc<RwLock<ServiceRegistry>>,
    pub cache_manager: Arc<RwLock<CacheManager>>,
}

#[derive(Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub uptime: u64,
}

#[get("/health")]
pub async fn health_check(state: web::Data<AppState>) -> Result<HttpResponse> {
    let response = HealthResponse {
        status: "healthy".to_string(),
        version: "2.0.0".to_string(),
        uptime: 12345,
    };

    Ok(HttpResponse::Ok().json(response))
}

#[derive(Serialize, Deserialize)]
pub struct AppResponse {
    pub id: String,
    pub name: String,
    pub icon: String,
}

#[get("/apps")]
pub async fn get_apps(state: web::Data<AppState>) -> Result<HttpResponse> {
    let app_manager = state.app_manager.read().await;
    let apps = app_manager.get_running_apps();

    let response: Vec<AppResponse> = apps
        .iter()
        .map(|app| AppResponse {
            id: app.id.clone(),
            name: app.name.clone(),
            icon: app.icon.clone(),
        })
        .collect();

    Ok(HttpResponse::Ok().json(response))
}

#[derive(Deserialize)]
pub struct LaunchRequest {
    pub app_id: String,
}

#[post("/apps/launch")]
pub async fn launch_app(
    state: web::Data<AppState>,
    req: web::Json<LaunchRequest>,
) -> Result<HttpResponse> {
    let mut app_manager = state.app_manager.write().await;
    
    match app_manager.launch_app(&req.app_id) {
        Ok(app) => Ok(HttpResponse::Ok().json(app)),
        Err(e) => Ok(HttpResponse::BadRequest().json(serde_json::json!({
            "error": e
        }))),
    }
}

#[derive(Deserialize)]
pub struct FileListRequest {
    pub path: String,
}

#[post("/files/list")]
pub async fn list_files(
    req: web::Json<FileListRequest>,
) -> Result<HttpResponse> {
    let file_service = FileService::new();
    
    match file_service.list_directory(&req.path) {
        Ok(files) => Ok(HttpResponse::Ok().json(files)),
        Err(e) => Ok(HttpResponse::BadRequest().json(serde_json::json!({
            "error": e
        }))),
    }
}

#[get("/files/{path:.*}")]
pub async fn get_file(path: web::Path<String>) -> Result<HttpResponse> {
    let file_service = FileService::new();
    let file_path = path.into_inner();
    
    match file_service.read_file(&file_path) {
        Ok(content) => Ok(HttpResponse::Ok().body(content)),
        Err(e) => Ok(HttpResponse::NotFound().json(serde_json::json!({
            "error": e
        }))),
    }
}

#[derive(Deserialize)]
pub struct UploadRequest {
    pub name: String,
    pub content: String,
}

#[post("/files/upload")]
pub async fn upload_file(
    req: web::Json<UploadRequest>,
) -> Result<HttpResponse> {
    let file_service = FileService::new();
    
    match file_service.write_file(&req.name, &req.content) {
        Ok(_) => Ok(HttpResponse::Created().json(serde_json::json!({
            "message": "File uploaded successfully",
            "name": req.name
        }))),
        Err(e) => Ok(HttpResponse::BadRequest().json(serde_json::json!({
            "error": e
        }))),
    }
}

#[get("/media")]
pub async fn get_media() -> Result<HttpResponse> {
    let media_service = MediaService::new();
    let playlist = media_service.get_playlist();

    Ok(HttpResponse::Ok().json(playlist))
}

#[get("/email/inbox")]
pub async fn get_email_inbox() -> Result<HttpResponse> {
    let email_service = EmailService::new();
    let inbox = email_service.get_inbox();

    Ok(HttpResponse::Ok().json(inbox))
}