//! The [`Ham`] struct &mdash; a thin wrapper around
//! [`holochain_client::AppWebsocket`] with built-in admin-interface
//! discovery, app-interface attach, signing credential authorization, and
//! typed msgpack zome calls.

use anyhow::{Context, Result};
use holochain_client::{
    AdminWebsocket, AppWebsocket, AuthorizeSigningCredentialsPayload, CellId, CellInfo,
    ClientAgentSigner, ExternIO, WebsocketConfig, ZomeCallTarget,
};
use serde::de::DeserializeOwned;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

/// Configuration for establishing a new [`Ham`] connection.
#[derive(Debug, Clone)]
pub struct HamConfig {
    /// Admin websocket port (conductor `admin_interfaces` entry).
    pub admin_port: u16,
    /// App websocket port to attach if no existing app interface is present.
    pub app_port: u16,
    /// Installed app id (`--installed-app-id`).
    pub app_id: String,
    /// Per-request timeout applied to the underlying `AppWebsocket`.
    /// Prevents a slow or hung zome call from blocking the caller
    /// indefinitely. Daemons typically set 60-120 seconds; one-shots can
    /// choose a shorter budget tied to their cron cadence.
    pub request_timeout_secs: u64,
}

impl HamConfig {
    /// Build a new [`HamConfig`] with the required fields. Equivalent to a
    /// builder-entry constructor &mdash; additional optional fields can be
    /// chained in future releases without breaking callers.
    pub fn new(admin_port: u16, app_port: u16, app_id: impl Into<String>) -> Self {
        Self {
            admin_port,
            app_port,
            app_id: app_id.into(),
            request_timeout_secs: 120,
        }
    }

    /// Override the per-request timeout (seconds).
    pub fn with_request_timeout_secs(mut self, secs: u64) -> Self {
        self.request_timeout_secs = secs;
        self
    }
}

/// A connected Holochain app websocket client.
///
/// Construct with [`Ham::connect`]. Use [`Ham::call_zome`] for typed
/// msgpack zome calls and [`Ham::ping`] as a lightweight health probe before
/// expensive multi-step cycles.
pub struct Ham {
    app_connection: AppWebsocket,
    cell_id: CellId,
    _signer: ClientAgentSigner,
}

impl Ham {
    /// Connect to the admin interface, attach an app interface if needed,
    /// issue an auth token, and open an authenticated app websocket with
    /// signing credentials for the first provisioned cell of `app_id`.
    ///
    /// The returned connection honors `cfg.request_timeout_secs` on every
    /// zome call.
    pub async fn connect(cfg: HamConfig) -> Result<Self> {
        info!(
            event = "ham.connecting",
            admin_port = cfg.admin_port,
            app_port = cfg.app_port,
            app_id = cfg.app_id.as_str(),
            request_timeout_secs = cfg.request_timeout_secs
        );

        let admin = AdminWebsocket::connect((Ipv4Addr::LOCALHOST, cfg.admin_port), None)
            .await
            .context("Failed to connect to admin interface")?;

        let app_interfaces = admin
            .list_app_interfaces()
            .await
            .context("Failed to list app interfaces")?;
        let app_interface = app_interfaces
            .iter()
            .find(|ai| ai.installed_app_id.is_none());
        let port = if let Some(ai) = app_interface {
            ai.port
        } else {
            admin
                .attach_app_interface(
                    cfg.app_port,
                    None,
                    holochain_client::AllowedOrigins::Any,
                    None,
                )
                .await
                .context("Failed to attach app interface")?
        };

        let issued_token = admin
            .issue_app_auth_token(cfg.app_id.clone().into())
            .await
            .context("Failed to issue app auth token")?;

        let mut ws_config = WebsocketConfig::CLIENT_DEFAULT;
        ws_config.default_request_timeout = Duration::from_secs(cfg.request_timeout_secs);
        let ws_config = Arc::new(ws_config);

        let signer = ClientAgentSigner::default();
        let app_connection = AppWebsocket::connect_with_config(
            (Ipv4Addr::LOCALHOST, port),
            ws_config,
            issued_token.token,
            signer.clone().into(),
            None,
        )
        .await
        .context("Failed to connect to app interface")?;

        let installed_app = app_connection
            .app_info()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get app info: {}", e))?
            .context("No app info found")?;
        let cells = installed_app
            .cell_info
            .into_values()
            .next()
            .context("No cells found in app")?;
        let cell_id = match cells.first().context("Empty cell list")? {
            CellInfo::Provisioned(c) => c.cell_id.clone(),
            _ => anyhow::bail!("Invalid cell type: expected Provisioned"),
        };

        let credentials = admin
            .authorize_signing_credentials(AuthorizeSigningCredentialsPayload {
                cell_id: cell_id.clone(),
                functions: None,
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to authorize signing credentials: {}", e))?;
        signer.add_credentials(cell_id.clone(), credentials);

        info!(event = "ham.connected");
        Ok(Self {
            app_connection,
            cell_id,
            _signer: signer,
        })
    }

    /// Call a zome function and decode the msgpack response into `R`.
    pub async fn call_zome<I, R>(
        &self,
        role_name: &str,
        zome_name: &str,
        fn_name: &str,
        payload: I,
    ) -> Result<R>
    where
        I: serde::Serialize + std::fmt::Debug,
        R: DeserializeOwned,
    {
        debug!(event = "ham.call_zome", role_name, zome_name, fn_name);
        let response = self
            .app_connection
            .call_zome(
                ZomeCallTarget::RoleName(role_name.to_string()),
                zome_name.into(),
                fn_name.into(),
                ExternIO::encode(payload)?,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to call zome: {}", e))?;
        rmp_serde::from_slice(&response.0).context("Failed to deserialize response")
    }

    /// Round-trip probe that surfaces a dead websocket immediately. Uses
    /// `app_info` rather than `cached_app_info` so it actually hits the
    /// conductor.
    pub async fn ping(&self) -> Result<()> {
        self.app_connection
            .app_info()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to probe app_info: {}", e))?;
        Ok(())
    }

    /// Fetch fresh app info from the conductor.
    pub async fn app_info(&self) -> Result<Option<holochain_client::AppInfo>> {
        self.app_connection
            .app_info()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get app info: {}", e))
    }

    /// The [`CellId`] of the first provisioned cell, captured at connect time.
    pub fn cell_id(&self) -> &CellId {
        &self.cell_id
    }
}
