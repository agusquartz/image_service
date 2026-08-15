mod auth;
mod config;
mod error;
mod health;
mod images;
mod router;
mod state;

use config::AppConfig;
use state::AppState;

use std::error::Error;

use tokio::{fs, net::TcpListener};

/// Application entry point.
///
/// Responsibilities here are intentionally limited to:
///
/// - Loading environment variables.
/// - Building the application configuration.
/// - Preparing the image storage directory.
/// - Creating the shared application state.
/// - Building the Axum router.
/// - Starting the HTTP server.
///
/// Business logic should not be added to this file.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();

    let config = AppConfig::from_env()?;

    // Ensure the root storage directory exists before accepting requests.
    fs::create_dir_all(&config.image_root).await?;

    let state = AppState::from_config(&config);

    let app = router::build_router(state);

    let listener = TcpListener::bind(&config.bind_address).await?;

    println!("Image service listening on {}", config.bind_address);

    println!("Image storage directory: {}", config.image_root.display());

    axum::serve(listener, app).await?;

    Ok(())
}
