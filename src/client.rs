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
/// [`HamConfig::with_lair_signing`] / [`HamConfig::with_lair_signing_from_node`].
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
    /// Skip `list_app_interfaces` discovery and always attach a fresh
    /// `AllowedOrigins::Any` app interface. Needed when the conductor has
    /// pre-existing unrestricted app interfaces whose token/origin state
    /// rejects our anonymous connect — the default discovery would keep
    /// re-picking the same stale one on every retry.
    pub force_fresh_attach: bool,
    /// When set, [`Ham::connect`] signs via lair (the cell's own agent key, no
    /// cap grant) instead of authorizing a throwaway signing key on chain.
    pub lair: Option<LairSigning>,
    /// Permission to sign by authorizing a throwaway key on chain, which
    /// commits one capability grant per connect. Without it and without
    /// [`HamConfig::lair`], [`Ham::connect`] refuses rather than write. Set by
    /// [`HamConfig::allow_cap_grant_signing`].
    pub allow_cap_grant_signing: bool,
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
            force_fresh_attach: false,
            lair: None,
            allow_cap_grant_signing: false,
        }
    }

    /// Override the per-request timeout (seconds).
    pub fn with_request_timeout_secs(mut self, secs: u64) -> Self {
        self.request_timeout_secs = secs;
        self
    }

    /// Always attach a fresh app interface, never reuse an existing one.
    pub fn with_force_fresh_attach(mut self, force: bool) -> Self {
        self.force_fresh_attach = force;
        self
    }

    /// Enable lair signing from an explicit connection URL + passphrase bytes
    /// (moved into locked memory; trailing newlines are stripped to match how
    /// the keystore was unlocked). Prefer
    /// [`HamConfig::with_lair_signing_from_node`] when the values live at the
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

    /// Enable lair signing by discovering the connection URL from a Holochain
    /// conductor config (`keystore.connection_url`) and the passphrase from
    /// `passphrase_file`. A node that cannot offer lair fails here, at config
    /// time and with the reason, instead of at a connect that would have
    /// written to the chain in its place.
    pub fn with_lair_signing_from_node(
        mut self,
        conductor_config_path: &Path,
        passphrase_file: &Path,
    ) -> Result<Self> {
        self.lair = Some(resolve_lair_from_node(
            conductor_config_path,
            passphrase_file,
        )?);
        Ok(self)
    }

    /// Best-effort [`HamConfig::with_lair_signing_from_node`]. On any failure,
    /// no external `lair_server` or unreadable files, this logs a warning and
    /// returns `self` with lair signing off, which leaves [`Ham::connect`]
    /// refusing unless [`HamConfig::allow_cap_grant_signing`] is also set.
    /// Pair the two to say "lair when the node has it, the chain write when it
    /// does not". Anything else wants the fallible form, which reports why
    /// lair was unavailable.
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
                "lair signing unavailable; Ham::connect refuses unless allow_cap_grant_signing is set"
            ),
        }
        self
    }

    /// Ask for the signing path that authorizes a throwaway key by committing
    /// a capability grant, for a caller that has weighed what that costs: on a
    /// chain that is already closed the grant is invalid, peers warrant the
    /// agent for it, and a warranted agent's signed close can never be served
    /// again. Lair, when configured, still wins: this is permission to write,
    /// not a request to.
    pub fn allow_cap_grant_signing(mut self) -> Self {
        self.allow_cap_grant_signing = true;
        self
    }
}

/// The error [`Ham::connect`] refuses a config with when every signing path
/// left would write to the agent's chain. Typed, because retrying it can never
/// succeed: only a config change can, and a caller looping on connect needs to
/// tell that apart from a conductor that is merely down. Classified by
/// [`crate::errors::is_signing_refusal`].
#[derive(Debug)]
pub struct SigningRefused(String);

impl std::fmt::Display for SigningRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SigningRefused {}

/// How [`Ham::connect`] signs zome calls, decided from the config before
/// anything is connected.
#[derive(Debug)]
enum Signing {
    /// Sign as the cell's own agent key through lair. Commits nothing.
    Lair(LairSigning),
    /// Authorize a throwaway signing key, committing one capability grant to
    /// the agent's chain per connect. Only reachable through
    /// [`HamConfig::allow_cap_grant_signing`].
    CapGrant,
}

impl Signing {
    /// Decide the signer, or refuse: with no lair and no opt-in, every signing
    /// path left writes to the agent's chain, and no caller asked for that.
    fn resolve(cfg: &HamConfig) -> Result<Self> {
        if let Some(lair) = cfg.lair.as_ref() {
            if cfg.allow_cap_grant_signing {
                // A caller whose own layer resolved "write to the chain" would
                // otherwise never learn that ham quietly did something safer.
                warn!(
                    event = "ham.cap_grant_unused",
                    "signing through lair: the config permits a capability grant but does not \
                     need one"
                );
            }
            return Ok(Self::Lair(lair.clone()));
        }
        if cfg.allow_cap_grant_signing {
            return Ok(Self::CapGrant);
        }
        Err(anyhow::Error::new(SigningRefused(format!(
            "refusing to connect to app `{}`: signing it would authorize a throwaway key by \
             committing a capability grant to the agent's chain, and nothing asked for that \
             write. On a chain that is already closed the grant is invalid, peers warrant the \
             agent for it, and its signed close can never be served again. Configure lair \
             signing (HamConfig::with_lair_signing / with_lair_signing_from_node) to sign with \
             the cell's own key and write nothing, or call \
             HamConfig::allow_cap_grant_signing() to ask for that write on purpose.",
            cfg.app_id
        ))))
    }

    /// The `signing` field on the `ham.connecting` / `ham.connected` events.
    fn label(&self) -> &'static str {
        match self {
            Self::Lair(_) => "lair",
            Self::CapGrant => "client",
        }
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
    /// key via lair and **no capability grant is committed**. Signing without
    /// lair instead authorizes a throwaway key on chain, one cap grant per
    /// connect, so it is reachable only through
    /// [`HamConfig::allow_cap_grant_signing`]. A config carrying neither is
    /// refused before anything is connected.
    ///
    /// The returned connection honors `cfg.request_timeout_secs` on every
    /// zome call.
    pub async fn connect(cfg: HamConfig) -> Result<Self> {
        // Before the first socket opens: a config that never asked to write to
        // the chain must not get as far as being able to.
        let signing = Signing::resolve(&cfg)?;

        info!(
            event = "ham.connecting",
            admin_port = cfg.admin_port,
            app_port = cfg.app_port,
            app_id = cfg.app_id.as_str(),
            request_timeout_secs = cfg.request_timeout_secs,
            signing = signing.label()
        );

        let admin = AdminWebsocket::connect((Ipv4Addr::LOCALHOST, cfg.admin_port), None)
            .await
            .context("Failed to connect to admin interface")?;

        let port = if cfg.force_fresh_attach {
            admin
                .attach_app_interface(
                    cfg.app_port,
                    None,
                    holochain_client::AllowedOrigins::Any,
                    None,
                )
                .await
                .context("Failed to attach fresh app interface")?
        } else {
            let app_interfaces = admin
                .list_app_interfaces()
                .await
                .context("Failed to list app interfaces")?;
            let app_interface = app_interfaces
                .iter()
                .find(|ai| ai.installed_app_id.is_none());
            if let Some(ai) = app_interface {
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
            }
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

        let (signer, pending): (DynAgentSigner, Pending) = match &signing {
            Signing::Lair(lair) => {
                // The cell lookup (admin) and the lair connection are
                // independent; run them concurrently — both feed
                // `add_credentials` afterwards.
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
                // Key the signer on the app's primary (first) provisioned cell
                // — the same cell the client path authorizes, and the one every
                // current (single-role) consumer calls. A multi-role app
                // signing a non-primary role would need per-cell registration
                // here.
                let mut signer = LairAgentSigner::new(Arc::new(lair_client));
                signer.add_credentials(cell_id.clone(), cell_id.agent_pubkey().clone());
                (Arc::new(signer), Pending::Lair(cell_id))
            }
            Signing::CapGrant => {
                let signer = ClientAgentSigner::default();
                (signer.clone().into(), Pending::Client(signer))
            }
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
                warn!(
                    event = "ham.cap_grant",
                    app_id = cfg.app_id.as_str(),
                    "committing a capability grant to the agent's chain, as asked for by \
                     allow_cap_grant_signing"
                );
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

        info!(event = "ham.connected", signing = signing.label());

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

/// A real refusal, straight from [`Ham::connect`], for the tests across this
/// crate that need one: they then cannot drift from what connect returns. The
/// refusal lands before any socket is opened, so the ports are never dialled,
/// and the bound wait makes a regression that dials them fail rather than hang.
#[cfg(test)]
pub(crate) async fn refused_connect_error() -> anyhow::Error {
    let refused = HamConfig::new(1, 1, "unyt");
    match tokio::time::timeout(Duration::from_secs(10), Ham::connect(refused))
        .await
        .expect("a refusal must not reach the conductor, let alone hang")
    {
        Ok(_) => panic!("a config with neither lair nor the opt-in must be refused"),
        Err(e) => e,
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
    use super::{parse_connection_url, strip_passphrase, Ham, HamConfig, Signing};
    use std::time::Duration;

    const LAIR_URL: &str = "unix:///var/lib/holochain/lair/socket?k=abc123";

    fn cfg() -> HamConfig {
        HamConfig::new(8800, 30000, "unyt")
    }

    /// A conductor config + passphrase file pair, laid out as
    /// `with_lair_signing_from_node` reads them off a node.
    fn node_with_lair() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("conductor-config.yaml"),
            format!("keystore:\n  type: lair_server\n  connection_url: {LAIR_URL}\n"),
        )
        .expect("write conductor config");
        std::fs::write(dir.path().join("lair-passphrase"), b"deadbeef\n")
            .expect("write lair passphrase");
        dir
    }

    /// A port with nothing listening on it: bound to learn a free one, then
    /// released as this returns.
    fn closed_port() -> u16 {
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("bind an ephemeral port")
            .local_addr()
            .expect("read the bound address")
            .port()
    }

    /// The error a connect against a conductor that isn't there fails with.
    /// Bounded, so a regression that hangs fails the test instead of the run.
    async fn connect_error(cfg: HamConfig) -> anyhow::Error {
        match tokio::time::timeout(Duration::from_secs(10), Ham::connect(cfg))
            .await
            .expect("connect must not hang")
        {
            Ok(_) => panic!("connect succeeded without a conductor"),
            Err(e) => e,
        }
    }

    #[test]
    fn a_config_that_asked_for_nothing_refuses_to_sign() {
        let err = Signing::resolve(&cfg())
            .expect_err("no lair and no opt-in must refuse")
            .to_string();
        assert!(err.contains("allow_cap_grant_signing"), "{err}");
        assert!(err.contains("unyt"), "{err}");
    }

    #[test]
    fn the_opt_in_selects_the_cap_grant_path() {
        let signing = Signing::resolve(&cfg().allow_cap_grant_signing())
            .expect("the opt-in is the one way to reach the cap-grant path");
        assert!(matches!(signing, Signing::CapGrant));
        assert_eq!(signing.label(), "client");
    }

    #[test]
    fn lair_signing_needs_no_opt_in() {
        let cfg = cfg()
            .with_lair_signing(LAIR_URL, b"deadbeef".to_vec())
            .expect("lair signing from an explicit URL");
        let signing = Signing::resolve(&cfg).expect("lair commits nothing, so it needs no opt-in");
        assert_eq!(signing.label(), "lair");
    }

    #[test]
    fn lair_wins_over_the_opt_in() {
        let cfg = cfg()
            .with_lair_signing(LAIR_URL, b"deadbeef".to_vec())
            .expect("lair signing from an explicit URL")
            .allow_cap_grant_signing();
        let signing = Signing::resolve(&cfg).expect("lair signing stays available");
        assert_eq!(
            signing.label(),
            "lair",
            "the opt-in permits the chain write, it does not ask for one"
        );
    }

    #[tokio::test]
    async fn connect_refuses_before_it_touches_the_conductor() {
        let cfg = HamConfig::new(closed_port(), 30000, "unyt");
        let err = connect_error(cfg).await;
        assert!(
            !crate::errors::is_connection_error(&err),
            "a refusal is a misconfiguration, not a transport failure a caller should retry: \
             {err:#}"
        );
        let err = format!("{err:#}");
        assert!(err.contains("refusing to connect"), "{err}");
        assert!(
            !err.contains("Failed to connect to admin interface"),
            "the refusal must land before any socket is opened: {err}"
        );
    }

    #[tokio::test]
    async fn the_opt_in_carries_connect_past_the_refusal() {
        let cfg = HamConfig::new(closed_port(), 30000, "unyt").allow_cap_grant_signing();
        let err = format!("{:#}", connect_error(cfg).await);
        assert!(
            err.contains("Failed to connect to admin interface"),
            "the opt-in must reach the conductor, where the cap grant would be committed: {err}"
        );
    }

    #[tokio::test]
    async fn lair_carries_connect_past_the_refusal() {
        let cfg = HamConfig::new(closed_port(), 30000, "unyt")
            .with_lair_signing(LAIR_URL, b"deadbeef".to_vec())
            .expect("lair signing from an explicit URL");
        let err = format!("{:#}", connect_error(cfg).await);
        assert!(
            err.contains("Failed to connect to admin interface"),
            "{err}"
        );
    }

    #[test]
    fn with_lair_signing_from_node_reads_the_node() {
        let dir = node_with_lair();
        let cfg = cfg()
            .with_lair_signing_from_node(
                &dir.path().join("conductor-config.yaml"),
                &dir.path().join("lair-passphrase"),
            )
            .expect("a node with an external lair_server configures lair signing");
        assert_eq!(
            cfg.lair
                .as_ref()
                .expect("lair signing")
                .connection_url
                .as_str(),
            LAIR_URL
        );
        assert!(
            !cfg.allow_cap_grant_signing,
            "reading a node's lair is not permission to write to its chain"
        );
    }

    #[test]
    fn with_lair_signing_from_node_fails_loudly() {
        let dir = node_with_lair();
        let missing = dir.path().join("absent-conductor-config.yaml");
        let err = cfg()
            .with_lair_signing_from_node(&missing, &dir.path().join("lair-passphrase"))
            .expect_err("a node without a conductor config cannot offer lair")
            .to_string();
        assert!(err.contains("absent-conductor-config.yaml"), "{err}");
    }

    #[test]
    fn try_lair_signing_from_node_leaves_the_config_refusing() {
        let dir = node_with_lair();
        let cfg = cfg().try_lair_signing_from_node(
            &dir.path().join("absent-conductor-config.yaml"),
            &dir.path().join("lair-passphrase"),
        );
        assert!(cfg.lair.is_none());
        assert!(
            Signing::resolve(&cfg).is_err(),
            "a failed discovery must not leave the caller on the chain-writing path"
        );
    }

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
        // Carries a connection_url, so the only thing that can reject it is the
        // type check itself.
        let cfg = "keystore:\n  type: danger_test_keystore\n  connection_url: unix:///x?k=y\n";
        let err = parse_connection_url(cfg)
            .expect_err("only an external lair_server exposes a connectable socket")
            .to_string();
        assert!(err.contains("danger_test_keystore"), "{err}");
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
