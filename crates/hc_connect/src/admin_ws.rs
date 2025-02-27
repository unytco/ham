use anyhow::{anyhow, Context, Result};
use holochain_conductor_api::{AdminRequest, AdminResponse, AppInfo};
use holochain_types::{
    app::{InstallAppPayload, InstalledAppId},
    websocket::AllowedOrigins,
};
use holochain_websocket::{connect, ConnectRequest, WebsocketConfig, WebsocketSender};
use std::{net::ToSocketAddrs, sync::Arc};
use tracing::trace;

#[derive(Clone)]
pub struct AdminWebsocket {
    tx: WebsocketSender,
}

impl AdminWebsocket {
    pub async fn connect(admin_port: u16) -> Result<Self> {
        let socket_addr = format!("127.0.0.1:{}", admin_port);
        let addr = socket_addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!("No socket address"))?;

        trace!("Connecting to holochain conductor at {}", socket_addr);

        let config = Arc::new(WebsocketConfig::CLIENT_DEFAULT);
        let (tx, _) = connect(config, ConnectRequest::new(addr)).await?;

        Ok(Self { tx })
    }

    pub async fn list_apps(&mut self) -> Result<Vec<AppInfo>> {
        let response = self
            .tx
            .request(AdminRequest::ListApps {
                status_filter: None,
            })
            .await
            .context("Failed to list apps")?;

        match response {
            AdminResponse::AppsListed(apps) => Ok(apps),
            _ => Err(anyhow!("Unexpected response")),
        }
    }

    pub async fn install_app(&mut self, payload: InstallAppPayload) -> Result<AppInfo> {
        let response = self
            .tx
            .request(AdminRequest::InstallApp(Box::new(payload)))
            .await
            .context("Failed to install app")?;

        match response {
            AdminResponse::AppInstalled(info) => Ok(info),
            AdminResponse::Error(err) => Err(anyhow!("Install failed: {:?}", err)),
            _ => Err(anyhow!("Unexpected response")),
        }
    }

    pub async fn enable_app(&mut self, installed_app_id: &InstalledAppId) -> Result<AppInfo> {
        let response = self
            .tx
            .request(AdminRequest::EnableApp {
                installed_app_id: installed_app_id.clone(),
            })
            .await
            .context("Failed to activate app")?;

        match response {
            AdminResponse::AppEnabled { app, errors: _ } => Ok(app),
            AdminResponse::Error(err) => Err(anyhow!("Activation failed: {:?}", err)),
            _ => Err(anyhow!("Unexpected response")),
        }
    }

    pub async fn attach_app_interface(
        &mut self,
        port: Option<u16>,
        allowed_origins: AllowedOrigins,
        installed_app_id: Option<InstalledAppId>,
    ) -> Result<u16> {
        let response = self
            .tx
            .request(AdminRequest::AttachAppInterface {
                port,
                allowed_origins,
                installed_app_id,
            })
            .await
            .context("Failed to attach app interface")?;

        match response {
            AdminResponse::AppInterfaceAttached { port, .. } => Ok(port),
            AdminResponse::Error(err) => Err(anyhow!("Attach failed: {:?}", err)),
            _ => Err(anyhow!("Unexpected response")),
        }
    }
}
