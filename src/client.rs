//! The [`Ham`] struct &mdash; a thin wrapper around
//! [`holochain_client::AppWebsocket`] with built-in admin-interface
//! discovery, app-interface attach, lair or client-side zome-call signing,
//! and typed msgpack zome calls.

use anyhow::{Context, Result};
use holochain_client::{
    AdminWebsocket, AppWebsocket, AuthorizeSigningCredentialsPayload, CellId, CellInfo,
    ClientAgentSigner, DynAgentSigner, ExternIO, LairAgentSigner, WebsocketConfig, ZomeCallTarget,
};
use lair_keystore_api::dependencies::sodoken::LockedArray;
use lair_keystore_api::dependencies::url::Url;
use lair_keystore_api::ipc_keystore_connect;
use lair_keystore_api::types::SharedLockedArray;
use serde::de::DeserializeOwned;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Lair connection details that make [`Ham::connect`] sign zome calls as the
/// cell's own agent key (the implicit `ChainAuthor` grant) instead of
/// authorizing a throwaway signing key on chain. Built by
/// [`HamConfig::with_lair_signing`] / [`HamConfig::try_lair_signing_from_node`].
#[derive(Clone)]
pub struct LairSigning {
    /// `lair_server` IPC connection URL (`unix://…?k=<server_pubkey>`).
    pub connection_url: Url,
    /// Passphrase that unlocks the lair connection, held in locked memory.
    pub passphrase: SharedLockedArray,
}

impl std::fmt::Debug for LairSigning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render the passphrase.
        f.debug_struct("LairSigning")
            .field("connection_url", &self.connection_url.as_str())
            .field("passphrase", &"<redacted>")
            .finish()
    }
}

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
    /// When set, [`Ham::connect`] signs via lair (the cell's own agent key, no
    /// cap grant) instead of authorizing a throwaway signing key on chain.
    pub lair: Option<LairSigning>,
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
            lair: None,
        }
    }

    /// Override the per-request timeout (seconds).
    pub fn with_request_timeout_secs(mut self, secs: u64) -> Self {
        self.request_timeout_secs = secs;
        self
    }

    /// Enable lair signing from an explicit connection URL + passphrase bytes
    /// (moved into locked memory; trailing newlines are stripped to match how
    /// the keystore was unlocked). Prefer
    /// [`HamConfig::try_lair_signing_from_node`] when the values live at the
    /// conductor's on-disk paths.
    pub fn with_lair_signing(mut self, connection_url: &str, passphrase: Vec<u8>) -> Result<Self> {
        let connection_url = Url::parse(connection_url)
            .with_context(|| format!("Invalid lair connection URL: {connection_url}"))?;
        self.lair = Some(LairSigning {
            connection_url,
            passphrase: lock_passphrase(passphrase),
        });
        Ok(self)
    }

    /// Try to enable lair signing by discovering the connection URL from a
    /// Holochain conductor config (`keystore.connection_url`) and the
    /// passphrase from `passphrase_file`. On any failure — no external
    /// `lair_server`, unreadable files — this logs a warning and returns
    /// `self` unchanged, so the caller falls back to the client-signing path
    /// (which commits a cap grant per connect) rather than failing outright.
    pub fn try_lair_signing_from_node(
        mut self,
        conductor_config_path: &Path,
        passphrase_file: &Path,
    ) -> Self {
        match resolve_lair_from_node(conductor_config_path, passphrase_file) {
            Ok(lair) => self.lair = Some(lair),
            Err(e) => warn!(
                event = "ham.lair_discovery_failed",
                conductor_config = %conductor_config_path.display(),
                passphrase_file = %passphrase_file.display(),
                error = %e,
                "lair signing unavailable; falling back to client signing (a cap grant is committed per connect)"
            ),
        }
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
    // Held to keep the signer — and, on the lair path, its keystore
    // connection — alive for the lifetime of the websocket.
    _signer: DynAgentSigner,
}

impl Ham {
    /// Connect to the admin interface, attach an app interface if needed,
    /// issue an auth token, and open an authenticated app websocket for the
    /// first provisioned cell of `app_id`.
    ///
    /// If `cfg.lair` is set, zome calls are signed with the cell's own agent
    /// key via lair and **no capability grant is committed**. Otherwise a
    /// throwaway signing key is authorized on chain (one cap grant per
    /// connect).
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

        // The lair path resolves the cell up front: the built-in
        // `LairAgentSigner` registers the agent key via `add_credentials`
        // (`&mut self`), which must run before the signer is wrapped in
        // `Arc<dyn>` and handed to connect — so the cell can't come from
        // post-connect app info. It commits no cap grant. The client path keeps
        // the original ordering — connect first, then authorize a throwaway key
        // on chain — so a *failed* connect commits nothing.
        enum Pending {
            /// Lair: cell already resolved, signer fully built.
            Lair(CellId),
            /// Client: authorize the on-chain grant once connect has succeeded.
            Client(ClientAgentSigner),
        }

        let (signer, pending): (DynAgentSigner, Pending) = if let Some(lair) = cfg.lair.as_ref() {
            // The cell lookup (admin) and the lair connection are independent;
            // run them concurrently — both feed `add_credentials` afterwards.
            let (cell_id, lair_client) =
                tokio::try_join!(cell_id_via_admin(&admin, &cfg.app_id), async {
                    ipc_keystore_connect(lair.connection_url.clone(), lair.passphrase.clone())
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "Failed to connect to lair keystore at {}: {}",
                                lair.connection_url,
                                e
                            )
                        })
                },)?;
            // Key the signer on the app's primary (first) provisioned cell —
            // the same cell the client path authorizes, and the one every
            // current (single-role) consumer calls. A multi-role app signing a
            // non-primary role would need per-cell registration here.
            let mut signer = LairAgentSigner::new(Arc::new(lair_client));
            signer.add_credentials(cell_id.clone(), cell_id.agent_pubkey().clone());
            (Arc::new(signer), Pending::Lair(cell_id))
        } else {
            let signer = ClientAgentSigner::default();
            (signer.clone().into(), Pending::Client(signer))
        };

        let app_connection = AppWebsocket::connect_with_config(
            (Ipv4Addr::LOCALHOST, port),
            ws_config,
            issued_token.token,
            signer.clone(),
            None,
        )
        .await
        .context("Failed to connect to app interface")?;

        let cell_id = match pending {
            Pending::Lair(cell_id) => cell_id,
            Pending::Client(client_signer) => {
                let cell_id = cell_id_via_app(&app_connection)?;
                let credentials = admin
                    .authorize_signing_credentials(AuthorizeSigningCredentialsPayload {
                        cell_id: cell_id.clone(),
                        functions: None,
                    })
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to authorize signing credentials: {}", e)
                    })?;
                client_signer.add_credentials(cell_id.clone(), credentials);
                cell_id
            }
        };

        info!(
            event = "ham.connected",
            signing = if cfg.lair.is_some() { "lair" } else { "client" }
        );

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

/// Reduce an [`AppInfo`] to the [`CellId`] of its first provisioned cell.
fn first_provisioned_cell(app_info: &holochain_client::AppInfo) -> Result<CellId> {
    let cells = app_info
        .cell_info
        .values()
        .next()
        .context("No cells found in app")?;
    match cells.first().context("Empty cell list")? {
        CellInfo::Provisioned(c) => Ok(c.cell_id.clone()),
        _ => anyhow::bail!("Invalid cell type: expected Provisioned"),
    }
}

/// Resolve `app_id`'s first provisioned [`CellId`] via the admin interface —
/// used by the lair path, which needs the cell before the app websocket opens.
async fn cell_id_via_admin(admin: &AdminWebsocket, app_id: &str) -> Result<CellId> {
    let app_info = admin
        .list_apps(None)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to list apps: {}", e))?
        .into_iter()
        .find(|app| app.installed_app_id == app_id)
        .with_context(|| format!("App `{app_id}` not installed"))?;
    first_provisioned_cell(&app_info)
}

/// Resolve the first provisioned [`CellId`] from the app info the websocket
/// cached at connect — used by the client path, with no extra round-trip.
fn cell_id_via_app(app: &AppWebsocket) -> Result<CellId> {
    first_provisioned_cell(app.cached_app_info())
}

/// Read the lair connection URL + passphrase from the conductor's on-disk
/// paths (see [`HamConfig::try_lair_signing_from_node`]).
fn resolve_lair_from_node(
    conductor_config_path: &Path,
    passphrase_file: &Path,
) -> Result<LairSigning> {
    let config_text = std::fs::read_to_string(conductor_config_path).with_context(|| {
        format!(
            "reading conductor config {}",
            conductor_config_path.display()
        )
    })?;
    let connection_url = parse_connection_url(&config_text)?;
    let raw = std::fs::read(passphrase_file)
        .with_context(|| format!("reading lair passphrase {}", passphrase_file.display()))?;
    Ok(LairSigning {
        connection_url,
        passphrase: lock_passphrase(raw),
    })
}

/// Pluck the lair `keystore.connection_url` from a Holochain conductor config.
/// Errors unless the keystore is an external `lair_server` — the only kind that
/// exposes a connectable socket.
fn parse_connection_url(config_text: &str) -> Result<Url> {
    use lair_keystore_api::dependencies::serde_yaml;
    let doc: serde_yaml::Value =
        serde_yaml::from_str(config_text).context("parsing conductor config YAML")?;
    let keystore = doc
        .get("keystore")
        .context("conductor config has no `keystore` section")?;
    let kind = keystore
        .get("type")
        .and_then(|t| t.as_str())
        .context("conductor config keystore has no `type`")?;
    anyhow::ensure!(
        kind == "lair_server",
        "conductor keystore type is `{kind}`, not `lair_server` — no external lair to connect to"
    );
    let url = keystore
        .get("connection_url")
        .and_then(|u| u.as_str())
        .context("conductor config keystore has no `connection_url`")?;
    Url::parse(url).with_context(|| format!("invalid lair connection_url `{url}`"))
}

/// Move passphrase bytes into locked memory as a [`SharedLockedArray`], after
/// stripping the trailing newline (see [`strip_passphrase`]).
fn lock_passphrase(bytes: Vec<u8>) -> SharedLockedArray {
    Arc::new(Mutex::new(LockedArray::from(strip_passphrase(bytes))))
}

/// Lair was unlocked at provisioning with the passphrase file's contents minus
/// trailing newline(s): heart writes the file with `openssl rand -hex 32 > …`
/// (a trailing `\n`) but unlocks with `printf '%s' "$(cat …)"`. We mirror that
/// exactly — strip trailing `\n` only (command substitution does not strip
/// `\r`), or `unlock` fails at connect.
fn strip_passphrase(mut bytes: Vec<u8>) -> Vec<u8> {
    while bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::{parse_connection_url, strip_passphrase};

    #[test]
    fn parse_connection_url_reads_lair_server_url() {
        let cfg = "\
keystore:
  type: lair_server
  connection_url: unix:///var/lib/holochain/lair/socket?k=abc123
data_root_path: /var/lib/holochain/data
";
        let url = parse_connection_url(cfg).expect("should parse a lair_server connection_url");
        assert_eq!(url.scheme(), "unix");
        assert!(url.as_str().contains("k=abc123"), "got {}", url.as_str());
    }

    #[test]
    fn parse_connection_url_rejects_non_lair_server() {
        let cfg = "keystore:\n  type: danger_test_keystore\n";
        assert!(parse_connection_url(cfg).is_err());
    }

    #[test]
    fn parse_connection_url_errors_without_url() {
        let cfg = "keystore:\n  type: lair_server\n";
        assert!(parse_connection_url(cfg).is_err());
    }

    #[test]
    fn parse_connection_url_errors_without_keystore() {
        let cfg = "data_root_path: /var/lib/holochain/data\n";
        assert!(parse_connection_url(cfg).is_err());
    }

    #[test]
    fn parse_connection_url_errors_without_type() {
        // A keystore section carrying a connection_url but no `type` must be
        // rejected, not assumed to be a lair_server.
        let cfg = "keystore:\n  connection_url: unix:///x?k=y\n";
        assert!(parse_connection_url(cfg).is_err());
    }

    #[test]
    fn strip_passphrase_drops_trailing_newlines_only() {
        assert_eq!(strip_passphrase(b"deadbeef\n".to_vec()), b"deadbeef");
        assert_eq!(strip_passphrase(b"deadbeef".to_vec()), b"deadbeef");
        assert_eq!(strip_passphrase(b"deadbeef\n\n".to_vec()), b"deadbeef");
        // `\r` is preserved — `$(cat)` strips trailing `\n` only, so lair would
        // have been unlocked with the `\r` still present.
        assert_eq!(strip_passphrase(b"deadbeef\r\n".to_vec()), b"deadbeef\r");
    }
}
