//! A real lair keystore for tests, reached through a socket the test cuts the
//! way a lair restart cuts every connection open to it.

use holochain_client::{AgentPubKey, AgentSigner, CellId, LairAgentSigner};
use lair_keystore_api::dependencies::hc_seed_bundle::PwHashLimits;
use lair_keystore_api::dependencies::sodoken::LockedArray;
use lair_keystore_api::ipc_keystore::{ipc_keystore_connect, IpcKeystoreServer};
use lair_keystore_api::mem_store::create_mem_store_factory;
use lair_keystore_api::prelude::{Ed25519PubKey, LairClient, LairServerConfigInner};
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{oneshot, Notify};
use tokio::task::JoinHandle;

pub(crate) struct Lair {
    pub client: Arc<LairClient>,
    pub key: Ed25519PubKey,
    /// Stops forwarding requests to lair; replies still come back.
    pub stall_requests: Arc<Notify>,
    forwarding: [JoinHandle<()>; 2],
    _server: IpcKeystoreServer,
    _dir: tempfile::TempDir,
}

impl Lair {
    pub async fn start() -> Self {
        let dir = tempfile::TempDir::with_prefix("ham-lair").unwrap();
        let passphrase = Arc::new(Mutex::new(LockedArray::from(b"passphrase".to_vec())));
        let config = PwHashLimits::Minimum
            .with_exec(|| LairServerConfigInner::new(dir.path(), passphrase.clone()))
            .await
            .unwrap();
        let server = IpcKeystoreServer::new(
            Arc::new(config),
            create_mem_store_factory(),
            passphrase.clone(),
        )
        .await
        .unwrap();

        let cut_path = dir.path().join("cut");
        let listener = UnixListener::bind(&cut_path).unwrap();
        let mut cut_url = server.get_config().connection_url.clone();
        cut_url.set_path(cut_path.to_str().unwrap());
        let lair_path = server.get_config().get_connection_path();
        let stall_requests = Arc::new(Notify::new());
        let stall = stall_requests.clone();
        let (forwarding_tx, forwarding_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (client, _) = listener.accept().await.unwrap();
            let lair = UnixStream::connect(lair_path).await.unwrap();
            let (mut from_client, mut to_client) = client.into_split();
            let (mut from_lair, mut to_lair) = lair.into_split();
            // Both halves are held after copying stops, so only `cut` closes
            // either socket.
            let requests = tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::io::copy(&mut from_client, &mut to_lair) => {}
                    _ = stall.notified() => {}
                }
                std::future::pending::<()>().await
            });
            let replies = tokio::spawn(async move {
                let _ = tokio::io::copy(&mut from_lair, &mut to_client).await;
                std::future::pending::<()>().await
            });
            let _ = forwarding_tx.send([requests, replies]);
        });

        let client = ipc_keystore_connect(cut_url, passphrase).await.unwrap();
        let key = client
            .new_seed("ham-test".into(), None, false)
            .await
            .unwrap()
            .ed25519_pub_key;
        Self {
            client: Arc::new(client),
            key,
            stall_requests,
            forwarding: forwarding_rx.await.unwrap(),
            _server: server,
            _dir: dir,
        }
    }

    pub async fn cut(&mut self) {
        for forwarding in &mut self.forwarding {
            forwarding.abort();
            let _ = forwarding.await;
        }
    }

    /// Sign as `holochain_client` signs a zome call through lair.
    pub fn sign(&self, key: Ed25519PubKey) -> impl Future<Output = anyhow::Result<()>> {
        let signer = LairAgentSigner::new(self.client.clone());
        async move {
            let agent = AgentPubKey::from_raw_32(key.0.to_vec());
            let cell = CellId::new(holo_hash::DnaHash::from_raw_32(vec![0; 32]), agent.clone());
            signer
                .sign(&cell, agent, Arc::from(&b"zome call"[..]))
                .await
                .map(drop)
        }
    }
}
