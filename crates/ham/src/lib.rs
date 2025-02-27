//! Ham (Holochain App Manager) provides utilities for managing Holochain applications.
//!
//! # Example
//! ```no_run
//! use ham::Ham;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let mut manager = Ham::connect(45678).await?;
//!     let app_info = manager.install_and_enable_with_default_agent("path/to/app.happ", None).await?;
//!     println!("Installed app: {:?}", app_info.installed_app_id);
//!     Ok(())
//! }
//! ```

use anyhow::{Context, Result};
use holochain_client::AdminWebsocket;
use holochain_conductor_api::AppInfo;
use holochain_types::{app::InstallAppPayload, prelude::NetworkSeed};
use std::path::Path;

/// Manages Holochain application installation and lifecycle
pub struct Ham {
    admin: AdminWebsocket,
}

impl Ham {
    /// Connect to a running Holochain conductor's admin interface
    pub async fn connect(admin_port: u16) -> Result<Self> {
        use std::net::Ipv4Addr;
        let admin = holochain_client::AdminWebsocket::connect((Ipv4Addr::LOCALHOST, admin_port))
            .await
            .context("Failed to connect to admin interface")?;

        Ok(Self { admin })
    }

    /// Install a .happ file with optional configuration
    pub async fn install_and_enable_with_default_agent<P: AsRef<Path>>(
        &mut self,
        happ_path: P,
        network_seed: Option<NetworkSeed>,
    ) -> Result<AppInfo> {
        // Generate a new agent key
        let agent_key = self
            .admin
            .generate_agent_pub_key()
            .await
            .expect("Failed to generate agent key");

        // Prepare installation payload
        let payload = InstallAppPayload {
            agent_key: Some(agent_key),
            installed_app_id: None,
            source: holochain_types::app::AppBundleSource::Path(happ_path.as_ref().to_path_buf()),
            network_seed,
            roles_settings: None,
            ignore_genesis_failure: false,
            allow_throwaway_random_agent_key: false,
        };

        // Install and enable the app
        let app_info = self
            .admin
            .install_app(payload)
            .await
            .expect("Failed to install app");
        self.admin
            .enable_app(app_info.installed_app_id.clone())
            .await
            .expect("Failed to enable app");

        Ok(app_info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holochain_env_setup::environment::setup_environment;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_app_installation() -> Result<()> {
        // Initialize logging for better debugging
        tracing_subscriber::fmt::init();
        // Create temporary directories for the test
        let tmp_dir = tempdir()?.into_path();
        let log_dir = tmp_dir.join("log");
        std::fs::create_dir_all(&log_dir)?;
        println!("Log directory created: {:?}", log_dir);
        // Setup the Holochain environment (starts conductor & lair)
        let _env = setup_environment(&tmp_dir, &log_dir, None, None).await?;
        println!("Environment setup complete...");
        // Wait a moment for the conductor to be ready
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let test_happ = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("ziptest.happ");

        println!("Connecting to admin interface...");
        let mut manager = Ham::connect(4444).await?;
        println!("Installing app {}...", test_happ.display());
        let app_info = manager
            .install_and_enable_with_default_agent(test_happ, None)
            .await?;
        println!("App installed: {:?}", app_info);
        assert!(!app_info.installed_app_id.is_empty());

        Ok(())
    }
}
