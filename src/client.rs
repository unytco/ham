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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Lair connection details that make [`Ham::connect`] sign zome calls as the
/// cell's own agent key (the implicit `ChainAuthor` grant) instead of
/// authorizing a throwaway signing key on chain.
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

    /// Set the lair signer field directly, from a connection URL and
    /// passphrase bytes (moved into locked memory; trailing newlines are
    /// stripped to match how the keystore was unlocked). The field-level
    /// primitive, for a caller assembling a config piece by piece;
    /// [`HamConfig::with_signing`] is the way in for a caller that wants the
    /// decision made for it.
    pub fn with_lair_signing(mut self, connection_url: &str, passphrase: Vec<u8>) -> Result<Self> {
        let connection_url = Url::parse(connection_url)
            .with_context(|| format!("Invalid lair connection URL: {connection_url}"))?;
        self.lair = Some(LairSigning {
            connection_url,
            passphrase: lock_passphrase(passphrase),
        });
        Ok(self)
    }

    /// Decide this config's signing path from the inputs the caller holds, and
    /// write the decision onto it. Name your own variables or flags as `anyhow`
    /// context on the error, so the naming stays where the names are. See
    /// [`SigningPolicy::resolve`] for the rules.
    pub fn with_signing(self, lair: LairCredentials, opt_in: CapGrantOptIn) -> Result<Self> {
        Ok(SigningPolicy::resolve(lair, opt_in)?.apply(self))
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

/// The error a signing decision fails with when nothing was offered to sign
/// with and every path left would write to the agent's chain. A credential that
/// was offered but cannot be used fails with a plain error instead, because it
/// names a specific thing to fix. Typed, because retrying it can never
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

/// The generic half of every refusal: the fault, carrying no consumer's
/// variable or flag names. Callers add those as `anyhow` context.
const LAIR_REQUIRED: &str = "lair signing is required and unavailable";

/// Where a caller's lair credentials come from. Consumers hold them in one of
/// two forms: a service whose installer rendered the values into its
/// environment already has them, and a process running beside the conductor
/// has the paths to read them off.
pub enum LairCredentials {
    /// Values the caller resolved itself. Both halves are needed; one without
    /// the other is a typo, not a signing path.
    Values {
        /// `lair_server` IPC connection URL (`unix://…?k=<server_pubkey>`).
        connection_url: Option<String>,
        /// Passphrase bytes that unlock it.
        passphrase: Option<Vec<u8>>,
    },
    /// The conductor's own files: the config whose `keystore.connection_url`
    /// names the keystore, and the file holding the passphrase.
    Node {
        conductor_config: PathBuf,
        passphrase_file: PathBuf,
    },
    /// The caller has no lair to offer. The same answer as `Values` with both
    /// halves absent, for a caller that has no lair concept at all.
    Absent,
}

impl std::fmt::Debug for LairCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Never render the passphrase.
            Self::Values { connection_url, .. } => f
                .debug_struct("Values")
                .field("connection_url", connection_url)
                .field("passphrase", &"<redacted>")
                .finish(),
            Self::Node {
                conductor_config,
                passphrase_file,
            } => f
                .debug_struct("Node")
                .field("conductor_config", conductor_config)
                .field("passphrase_file", passphrase_file)
                .finish(),
            Self::Absent => f.write_str("Absent"),
        }
    }
}

/// What reading a caller's credentials produced. The two failures are
/// different questions: whether there is a lair to reach, and whether what the
/// caller handed over can be used. Only the first is what the opt-in is for.
enum Offered {
    /// A usable signer.
    Signer(LairSigning),
    /// No lair to sign with, through no fault of the caller's input: it offered
    /// none, or the node it named has none to reach. Carries the reason where
    /// there is one.
    NoLair(Option<anyhow::Error>),
    /// The caller offered something that cannot be used.
    Unusable(anyhow::Error),
}

impl LairCredentials {
    fn resolve(self) -> Offered {
        match self {
            Self::Absent
            | Self::Values {
                connection_url: None,
                passphrase: None,
            } => Offered::NoLair(None),
            Self::Values {
                connection_url: Some(connection_url),
                passphrase: Some(passphrase),
            } => {
                // Parsed here rather than at connect: left to the connection an
                // unusable URL surfaces as a transport failure, which a
                // supervised loop retries forever.
                match Url::parse(&connection_url) {
                    Ok(url) => Offered::Signer(LairSigning {
                        connection_url: url,
                        // An empty passphrase is accepted deliberately: lair
                        // can be provisioned with one, so rejecting it here
                        // would refuse a node that does work.
                        passphrase: lock_passphrase(passphrase),
                    }),
                    Err(e) => Offered::Unusable(anyhow::Error::new(e).context(format!(
                        "the lair connection URL is not a URL: `{connection_url}`"
                    ))),
                }
            }
            Self::Values { connection_url, .. } => Offered::Unusable(anyhow::anyhow!(
                "{}",
                if connection_url.is_some() {
                    "the lair connection URL is set and its passphrase is not"
                } else {
                    "the lair passphrase is set and its connection URL is not"
                }
            )),
            Self::Node {
                conductor_config,
                passphrase_file,
            } => match resolve_lair_from_node(&conductor_config, &passphrase_file) {
                Ok(signer) => Offered::Signer(signer),
                Err(e) => Offered::NoLair(Some(e)),
            },
        }
    }
}

/// Whether the caller will accept the signing path that authorizes a throwaway
/// key by committing a capability grant to the agent's chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapGrantOptIn {
    /// Not asked for: lair, or a refusal.
    Withheld,
    /// Permission to sign that way where there is no other way. Lair wins
    /// wherever it is available, so this is never a request to write.
    Permitted,
}

impl CapGrantOptIn {
    /// Read the opt-in from an operator-supplied value. `1`, `true`, `yes` and
    /// `on` are [`CapGrantOptIn::Permitted`]; `0`, `false`, `no`, `off`, an
    /// empty value and an absent one are [`CapGrantOptIn::Withheld`]. Anything
    /// else is an error rather than a guess: the value permits the path that
    /// writes to the chain, so it is never inferred. Name the variable it came
    /// from as `anyhow` context.
    pub fn from_value(raw: Option<&str>) -> Result<Self> {
        let Some(value) = raw else {
            return Ok(Self::Withheld);
        };
        let value = value.trim();
        if ["1", "true", "yes", "on"]
            .iter()
            .any(|on| value.eq_ignore_ascii_case(on))
        {
            return Ok(Self::Permitted);
        }
        if ["", "0", "false", "no", "off"]
            .iter()
            .any(|off| value.eq_ignore_ascii_case(off))
        {
            return Ok(Self::Withheld);
        }
        anyhow::bail!("expected one of 1/true/yes/on or 0/false/no/off, got `{value}`")
    }

    /// Read the opt-in from a flag the caller already parsed.
    pub fn from_flag(set: bool) -> Self {
        if set {
            Self::Permitted
        } else {
            Self::Withheld
        }
    }
}

/// How zome calls will be signed, decided before anything is connected so that
/// a caller which never asked to write to the chain never gets as far as being
/// able to. Resolve it once from the caller's inputs and [`SigningPolicy::apply`]
/// it to every [`HamConfig`] a connection is built from.
#[derive(Debug, Clone)]
pub struct SigningPolicy(Decision);

#[derive(Debug, Clone)]
enum Decision {
    /// Sign as the cell's own agent key through lair. Commits nothing.
    Lair(LairSigning),
    /// Authorize a throwaway signing key, committing one capability grant to
    /// the agent's chain per connect.
    CapGrant,
}

impl SigningPolicy {
    /// Decide the signing path from what the caller has: its lair credentials
    /// in whichever form it holds them, and whether it permits the capability
    /// grant.
    ///
    /// Lair wins wherever it resolves. [`CapGrantOptIn::Permitted`] then covers
    /// exactly one case: there is no lair to reach. A credential the caller
    /// supplied and got wrong is a misconfiguration to fix, so it is fatal
    /// whichever way the opt-in is set; a typo is not "no other way", and
    /// falling back on one would write to the chain because of a typo.
    ///
    /// Fails with [`SigningRefused`] where nothing was offered and the opt-in
    /// is withheld, because every signing path left writes to the agent's
    /// chain.
    pub fn resolve(lair: LairCredentials, opt_in: CapGrantOptIn) -> Result<Self> {
        let unavailable = match lair.resolve() {
            Offered::Signer(signer) => return Self::decide(Some(signer), opt_in),
            Offered::Unusable(e) => return Err(e.context(LAIR_REQUIRED)),
            Offered::NoLair(why) => why,
        };
        match (opt_in, unavailable) {
            (CapGrantOptIn::Withheld, Some(e)) => Err(e.context(LAIR_REQUIRED)),
            (CapGrantOptIn::Withheld, None) => Self::decide(None, opt_in),
            (CapGrantOptIn::Permitted, why) => {
                if let Some(e) = why {
                    warn!(
                        event = "ham.lair_unavailable",
                        error = %format!("{e:#}"),
                        "no lair to reach; taking the capability grant this caller permitted"
                    );
                }
                Self::decide(None, opt_in)
            }
        }
    }

    /// The decision [`Ham::connect`] makes for a config assembled field by
    /// field rather than through [`SigningPolicy::resolve`]. A config the
    /// policy wrote, and has not been edited since, decides the same way here.
    fn from_config(cfg: &HamConfig) -> Result<Self> {
        Self::decide(
            cfg.lair.clone(),
            CapGrantOptIn::from_flag(cfg.allow_cap_grant_signing),
        )
    }

    /// Every signing decision comes through here.
    fn decide(signer: Option<LairSigning>, opt_in: CapGrantOptIn) -> Result<Self> {
        if let Some(signer) = signer {
            if opt_in == CapGrantOptIn::Permitted {
                // The expected outcome for a caller that carries the permission
                // permanently, so it reports rather than warns.
                info!(
                    event = "ham.cap_grant_unused",
                    "signing through lair: the caller permits a capability grant and none is \
                     needed"
                );
            }
            return Ok(Self(Decision::Lair(signer)));
        }
        if opt_in == CapGrantOptIn::Withheld {
            return Err(anyhow::Error::new(SigningRefused(format!(
                "refusing to connect: {LAIR_REQUIRED}, and the only signing path left authorizes \
                 a throwaway key by committing a capability grant to this agent's chain, which \
                 nothing asked for. On a chain that is already closed that action is invalid, \
                 peers warrant the agent for it, and its signed close can never be served again. \
                 Supply lair credentials to sign with the cell's own key and write nothing, or \
                 permit that write on purpose with CapGrantOptIn::Permitted."
            ))));
        }
        Ok(Self(Decision::CapGrant))
    }

    /// Write the decision onto a config, so it carries the decision and nothing
    /// stale can be read out of it.
    pub fn apply(&self, mut cfg: HamConfig) -> HamConfig {
        match &self.0 {
            Decision::Lair(signer) => {
                cfg.lair = Some(signer.clone());
                cfg.allow_cap_grant_signing = false;
            }
            Decision::CapGrant => {
                cfg.lair = None;
                cfg.allow_cap_grant_signing = true;
            }
        }
        cfg
    }

    /// The `signing` field on the `ham.connecting` / `ham.connected` events.
    pub fn label(&self) -> &'static str {
        match self.0 {
            Decision::Lair(_) => "lair",
            Decision::CapGrant => "client",
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
    /// refused before anything is connected. [`HamConfig::with_signing`]
    /// decides all of that from a caller's own inputs.
    ///
    /// The returned connection honors `cfg.request_timeout_secs` on every
    /// zome call.
    pub async fn connect(cfg: HamConfig) -> Result<Self> {
        // Before the first socket opens: a config that never asked to write to
        // the chain must not get as far as being able to.
        let signing =
            SigningPolicy::from_config(&cfg).with_context(|| format!("app `{}`", cfg.app_id))?;

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

        let (signer, pending): (DynAgentSigner, Pending) = match &signing.0 {
            Decision::Lair(lair) => {
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
            Decision::CapGrant => {
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
/// paths (see [`LairCredentials::Node`]).
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
    use super::{
        parse_connection_url, strip_passphrase, CapGrantOptIn, Ham, HamConfig, LairCredentials,
        SigningPolicy,
    };
    use std::time::Duration;

    const LAIR_URL: &str = "unix:///var/lib/holochain/lair/socket?k=abc123";

    fn cfg() -> HamConfig {
        HamConfig::new(8800, 30000, "unyt")
    }

    /// A conductor config + passphrase file pair, laid out as
    /// [`LairCredentials::Node`] reads them off a node.
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

    fn values(connection_url: Option<&str>, passphrase: Option<&str>) -> LairCredentials {
        LairCredentials::Values {
            connection_url: connection_url.map(str::to_string),
            passphrase: passphrase.map(|p| p.as_bytes().to_vec()),
        }
    }

    fn node(dir: &tempfile::TempDir, conductor_config: &str) -> LairCredentials {
        LairCredentials::Node {
            conductor_config: dir.path().join(conductor_config),
            passphrase_file: dir.path().join("lair-passphrase"),
        }
    }

    #[test]
    fn a_config_that_asked_for_nothing_refuses_to_sign() {
        let err = SigningPolicy::from_config(&cfg())
            .expect_err("no lair and no opt-in must refuse")
            .to_string();
        assert!(err.contains("refusing to connect"), "{err}");
        assert!(err.contains("CapGrantOptIn"), "{err}");
    }

    #[test]
    fn the_opt_in_selects_the_cap_grant_path() {
        let signing = SigningPolicy::from_config(&cfg().allow_cap_grant_signing())
            .expect("the opt-in is the one way to reach the cap-grant path");
        assert_eq!(signing.label(), "client");
    }

    #[test]
    fn lair_signing_needs_no_opt_in() {
        let cfg = cfg()
            .with_lair_signing(LAIR_URL, b"deadbeef".to_vec())
            .expect("lair signing from an explicit URL");
        let signing =
            SigningPolicy::from_config(&cfg).expect("lair commits nothing, so it needs no opt-in");
        assert_eq!(signing.label(), "lair");
    }

    #[test]
    fn lair_wins_over_the_opt_in() {
        let cfg = cfg()
            .with_lair_signing(LAIR_URL, b"deadbeef".to_vec())
            .expect("lair signing from an explicit URL")
            .allow_cap_grant_signing();
        let signing = SigningPolicy::from_config(&cfg).expect("lair signing stays available");
        assert_eq!(
            signing.label(),
            "lair",
            "the opt-in permits the chain write, it does not ask for one"
        );
    }

    #[test]
    fn supplied_values_reach_the_lair_signer() {
        let cfg = cfg()
            .with_signing(
                values(Some(LAIR_URL), Some("pass")),
                CapGrantOptIn::Withheld,
            )
            .expect("both halves of the credentials configure lair signing");
        // The URL itself, not just "some lair": the keystore it reaches is the
        // one whose key the cell is signed with.
        assert_eq!(
            cfg.lair.expect("lair signing").connection_url.as_str(),
            LAIR_URL
        );
        assert!(
            !cfg.allow_cap_grant_signing,
            "supplying credentials is not permission to write to the chain"
        );
    }

    #[test]
    fn a_nodes_paths_reach_the_lair_signer() {
        let dir = node_with_lair();
        let cfg = cfg()
            .with_signing(node(&dir, "conductor-config.yaml"), CapGrantOptIn::Withheld)
            .expect("a node with an external lair_server configures lair signing");
        assert_eq!(
            cfg.lair.expect("lair signing").connection_url.as_str(),
            LAIR_URL
        );
        assert!(!cfg.allow_cap_grant_signing);
    }

    #[test]
    fn half_the_values_is_not_a_signing_path() {
        for (connection_url, passphrase, expected) in [
            (Some(LAIR_URL), None, "connection URL is set"),
            (None, Some("pass"), "passphrase is set"),
        ] {
            let err = format!(
                "{:#}",
                SigningPolicy::resolve(values(connection_url, passphrase), CapGrantOptIn::Withheld)
                    .expect_err("half the credentials cannot sign anything")
            );
            assert!(err.contains("lair signing is required"), "{err}");
            // Which half is missing, so a caller naming its own two variables
            // does not send an operator looking at both.
            assert!(err.contains(expected), "{err}");
        }
    }

    #[test]
    fn no_credentials_and_no_opt_in_refuses() {
        for lair in [LairCredentials::Absent, values(None, None)] {
            let err = SigningPolicy::resolve(lair, CapGrantOptIn::Withheld)
                .expect_err("nothing offered and nothing permitted must refuse");
            assert!(crate::errors::is_signing_refusal(&err), "{err:#}");
        }
    }

    #[test]
    fn a_malformed_lair_url_is_fatal_at_resolve_not_at_connect() {
        let err = format!(
            "{:#}",
            SigningPolicy::resolve(
                values(Some("not a url"), Some("pass")),
                CapGrantOptIn::Withheld
            )
            .expect_err("an unusable URL must not be left for the connection to discover")
        );
        assert!(err.contains("not a url"), "{err}");
    }

    #[test]
    fn the_opt_in_reads_only_explicit_answers() {
        for raw in ["1", "true", "YES", "On", " 1 ", "\ttrue\n"] {
            assert_eq!(
                CapGrantOptIn::from_value(Some(raw)).expect("an affirmative"),
                CapGrantOptIn::Permitted,
                "{raw}"
            );
        }
        for raw in ["0", "false", "no", "OFF", ""] {
            assert_eq!(
                CapGrantOptIn::from_value(Some(raw)).expect("a negative"),
                CapGrantOptIn::Withheld,
                "{raw}"
            );
        }
        assert_eq!(
            CapGrantOptIn::from_value(None).expect("an absent value"),
            CapGrantOptIn::Withheld
        );
    }

    #[test]
    fn an_unrecognized_opt_in_value_is_an_error_not_a_guess() {
        let err = CapGrantOptIn::from_value(Some("maybe"))
            .expect_err("the value permits a chain write, so it is never inferred")
            .to_string();
        assert!(err.contains("maybe"), "{err}");
    }

    #[test]
    fn the_flag_form_of_the_opt_in_matches_the_value_form() {
        assert_eq!(CapGrantOptIn::from_flag(true), CapGrantOptIn::Permitted);
        assert_eq!(CapGrantOptIn::from_flag(false), CapGrantOptIn::Withheld);
    }

    #[test]
    fn the_permitted_opt_in_still_prefers_lair() {
        let dir = node_with_lair();
        let cfg = cfg()
            .with_signing(
                node(&dir, "conductor-config.yaml"),
                CapGrantOptIn::Permitted,
            )
            .expect("lair signing stays available");
        assert_eq!(
            cfg.lair.expect("lair signing").connection_url.as_str(),
            LAIR_URL,
            "the opt-in permits the chain write, it does not ask for one"
        );
        assert!(
            !cfg.allow_cap_grant_signing,
            "a config the policy wrote must carry the decision, not the permission"
        );
    }

    #[test]
    fn applying_a_policy_overwrites_whatever_the_config_was_carrying() {
        let dir = node_with_lair();
        // Onto a config that already permitted the chain write: the decision
        // replaces both fields, so a config can never report a permission the
        // policy did not grant or a signer it did not choose.
        let decided = SigningPolicy::resolve(
            node(&dir, "conductor-config.yaml"),
            CapGrantOptIn::Permitted,
        )
        .expect("a node with lair resolves to lair")
        .apply(cfg().allow_cap_grant_signing());
        assert!(decided.lair.is_some());
        assert!(!decided.allow_cap_grant_signing);

        // And the other way: a cap-grant decision clears a lair signer the
        // config was carrying.
        let decided = SigningPolicy::resolve(LairCredentials::Absent, CapGrantOptIn::Permitted)
            .expect("the opt-in permits the fallback")
            .apply(
                cfg()
                    .with_lair_signing(LAIR_URL, b"deadbeef".to_vec())
                    .expect("lair signing from an explicit URL"),
            );
        assert!(decided.lair.is_none());
        assert!(decided.allow_cap_grant_signing);
    }

    #[test]
    fn the_permitted_opt_in_is_the_fallback_when_lair_is_unavailable() {
        let dir = node_with_lair();
        let cfg = cfg()
            .with_signing(
                node(&dir, "absent-conductor-config.yaml"),
                CapGrantOptIn::Permitted,
            )
            .expect("the caller permitted the cap-grant path for exactly this case");
        assert!(cfg.lair.is_none());
        assert!(cfg.allow_cap_grant_signing);
    }

    #[test]
    fn a_failed_discovery_without_the_opt_in_never_reaches_the_chain_writing_path() {
        let dir = node_with_lair();
        let err = format!(
            "{:#}",
            cfg()
                .with_signing(
                    node(&dir, "absent-conductor-config.yaml"),
                    CapGrantOptIn::Withheld,
                )
                .expect_err("a node that cannot offer lair has no signing path left")
        );
        assert!(err.contains("lair signing is required"), "{err}");
        assert!(err.contains("absent-conductor-config.yaml"), "{err}");
    }

    #[test]
    fn a_policy_decides_the_same_way_again_through_the_config_it_wrote() {
        let dir = node_with_lair();
        for (lair, opt_in) in [
            (node(&dir, "conductor-config.yaml"), CapGrantOptIn::Withheld),
            (
                node(&dir, "conductor-config.yaml"),
                CapGrantOptIn::Permitted,
            ),
            (
                node(&dir, "absent-conductor-config.yaml"),
                CapGrantOptIn::Permitted,
            ),
        ] {
            let policy = SigningPolicy::resolve(lair, opt_in).expect("a decidable policy");
            let again = SigningPolicy::from_config(&policy.apply(cfg()))
                .expect("a config the policy wrote is never refused");
            assert_eq!(policy.label(), again.label(), "{opt_in:?}");
        }
    }

    #[test]
    fn the_debug_of_supplied_credentials_never_renders_the_passphrase() {
        let rendered = format!("{:?}", values(Some(LAIR_URL), Some("s3cr3t")));
        assert!(!rendered.contains("s3cr3t"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[test]
    fn the_debug_of_a_resolved_signer_never_renders_the_passphrase() {
        // The form that outlives `resolve`: every consumer holds it inside a
        // `HamConfig`, and some hold the policy too, so both are one `{:?}`
        // away from a log line.
        let policy = SigningPolicy::resolve(
            values(Some(LAIR_URL), Some("s3cr3t")),
            CapGrantOptIn::Withheld,
        )
        .expect("both halves configure lair signing");
        for rendered in [format!("{policy:?}"), format!("{:?}", policy.apply(cfg()))] {
            assert!(!rendered.contains("s3cr3t"), "{rendered}");
            assert!(rendered.contains("redacted"), "{rendered}");
        }
    }

    #[test]
    fn a_credential_that_cannot_be_used_is_fatal_even_where_the_opt_in_is_granted() {
        // The opt-in covers "this node has no lair to reach". It does not cover
        // a typo: falling back on one would commit a capability grant to the
        // agent's chain because a character was wrong.
        for (connection_url, passphrase) in [
            (Some("not a url"), Some("pass")),
            (Some(LAIR_URL), None),
            (None, Some("pass")),
        ] {
            let err = format!(
                "{:#}",
                SigningPolicy::resolve(
                    values(connection_url, passphrase),
                    CapGrantOptIn::Permitted,
                )
                .expect_err("a supplied credential that cannot be used is never a chain write")
            );
            assert!(err.contains("lair signing is required"), "{err}");
        }
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
        assert!(err.contains("unyt"), "the refusal must name the app: {err}");
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
