/**
 * Gaia Main Entry Point
 * File: gaia/src/main.rs
 * Purpose: Application bootstrap and web server setup
 */

use actix_web::{web, App, HttpServer, middleware};
use tokio::sync::RwLock;
use std::sync::Arc;
use log::info;

mod lib;
mod managers;
mod services;
mod taskbar;
mod gpu_renderer;
mod wayland_optimizer;
mod cache_manager;
mod app_loader;
mod file_service;
mod media_service;
mod document_service;
mod email_service;
mod ui;
mod support;

use crate::managers::AppManager;
use crate::services::ServiceRegistry;
use crate::cache_manager::CacheManager;
use crate::ui::api::AppState;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    info!("🚀 Gaia Shell v2.0 Starting...");

    // Initialize managers
    let app_manager = Arc::new(RwLock::new(AppManager::new()));
    let service_registry = Arc::new(RwLock::new(ServiceRegistry::new()));
    let cache_manager = Arc::new(RwLock::new(CacheManager::new(1000)));

    // Create shared state
    let state = web::Data::new(AppState {
        app_manager: Arc::clone(&app_manager),
        service_registry: Arc::clone(&service_registry),
        cache_manager: Arc::clone(&cache_manager),
    });

    info!("📦 Initializing services...");
    {
        let mut registry = service_registry.write().await;
        registry.register_service("file-service", Box::new(file_service::FileService::new()));
        registry.register_service("media-service", Box::new(media_service::MediaService::new()));
        registry.register_service("document-service", Box::new(document_service::DocumentService::new()));
        registry.register_service("email-service", Box::new(email_service::EmailService::new()));
    }

    info!("🎨 Initializing GPU renderer...");
    gpu_renderer::GPURenderer::init();

    info!("🌊 Initializing Wayland optimizer...");
    wayland_optimizer::WaylandOptimizer::init();

    info!("🚀 Starting HTTP server on http://localhost:3000");

    HttpServer::new(move || {
        App::new()
            .app_data(state.clone())
            .wrap(middleware::Logger::default())
            .wrap(middleware::NormalizePath::trim())
            .service(web::scope("/api")
                .service(ui::api::health_check)
                .service(ui::api::get_apps)
                .service(ui::api::launch_app)
                .service(ui::api::list_files)
                .service(ui::api::get_file)
                .service(ui::api::upload_file)
                .service(ui::api::get_media)
                .service(ui::api::get_email_inbox)
            )
            .service(actix_files::Files::new("/", "./static")
                .index_file("index.html")
            )
    })
    .bind("127.0.0.1:3000")?
    .run()
    .await
}
