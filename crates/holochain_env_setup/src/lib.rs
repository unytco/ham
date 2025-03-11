//! # holochain_env_setup
//!
//! Test utilities for setting up Holochain environments with conductor and lair-keystore.
//!
//! This crate provides utilities for setting up a complete Holochain environment for testing purposes.
//!
//! ## Example
//!
//! ```no_run
//! use holochain_env_setup::environment::setup_environment;
//! use tempfile::tempdir;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Create temporary directories
//!     let tmp_dir = tempdir()?.into_path();
//!     let log_dir = tmp_dir.join("log");
//!     std::fs::create_dir_all(&log_dir)?;
//!
//!     // Setup the environment
//!     let env = setup_environment(&tmp_dir, &log_dir, None, None).await?;
//!
//!     // Use the environment...
//!     let _agent_key = env.keystore.new_sign_keypair_random().await?;
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Features
//!
//! - Temporary environment setup for testing
//! - Automatic process cleanup
//! - Configurable ports and settings
//! - Integration with Lair keystore
//! - Logging support

pub mod environment;
pub mod holochain;
pub mod lair;
pub mod storage_helpers;
pub mod taskgroup_manager;

// Re-export commonly used items
pub use environment::setup_environment;
pub use environment::Environment;

#[cfg(test)]
mod tests {
    use super::*;
    use environment::setup_environment;
    use tempfile::tempdir;
    use tracing::info;

    #[tokio::test]
    async fn test_environment_setup() -> Result<(), Box<dyn std::error::Error>> {
        // Initialize logging for better debugging
        tracing_subscriber::fmt::init();

        info!("Creating temporary directories...");
        let tmp_dir = tempdir()?.into_path();
        let log_dir = tmp_dir.join("log");
        std::fs::create_dir_all(&log_dir)?;

        info!("Setting up Holochain environment...");
        let env = setup_environment(&tmp_dir, &log_dir, None, None).await?;

        // Wait a moment for the conductor to be ready
        info!("Waiting for conductor to initialize...");
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        // Test admin interface connection
        info!("Testing admin interface...");
        use std::net::Ipv4Addr;
        let admin = holochain_client::AdminWebsocket::connect((Ipv4Addr::LOCALHOST, 4444)).await?;
        let apps = admin.list_apps(None).await.expect("Failed to list apps");
        info!("Successfully listed apps: {:?}", apps);

        // Test lair keystore
        info!("Testing lair keystore...");
        let _agent_key = env.keystore.new_sign_keypair_random().await?;
        info!("Successfully generated agent key");

        Ok(())
    }
}
