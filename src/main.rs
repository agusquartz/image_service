use std::error::Error;

/// Application entry point.
///
/// Responsibilities here are intentionally limited to:
///
/// - Loading `.env` for local runs.
/// - Delegating service startup to the reusable Core crate.
///
/// Business logic should not be added to this file.
fn main() -> Result<(), Box<dyn Error>> {
    dotenvy::dotenv().ok();
    file_core::run()
}
