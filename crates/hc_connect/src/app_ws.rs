use anyhow::{anyhow, Context, Result};
use holochain_conductor_api::{AppRequest, AppResponse, ZomeCall};
use holochain_keystore::MetaLairClient;
use holochain_types::prelude::*;
use holochain_websocket::{connect, ConnectRequest, WebsocketConfig, WebsocketSender};
use serde::de::DeserializeOwned;
use std::{net::ToSocketAddrs, sync::Arc};
use tracing::trace;

#[derive(Clone)]
pub struct AppWebsocket {
    tx: WebsocketSender,
    keystore: MetaLairClient,
}

impl AppWebsocket {
    pub async fn connect(app_port: u16, keystore: MetaLairClient) -> Result<Self> {
        let socket_addr = format!("127.0.0.1:{}", app_port);
        let addr = socket_addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!("No socket address"))?;

        trace!("Connecting to app interface at {}", socket_addr);

        let config = Arc::new(WebsocketConfig::CLIENT_DEFAULT);
        let (tx, _) = connect(config, ConnectRequest::new(addr)).await?;

        Ok(Self { tx, keystore })
    }

    pub async fn call_zome<I, O>(
        &mut self,
        cell_id: CellId,
        zome_name: ZomeName,
        fn_name: FunctionName,
        payload: I,
    ) -> Result<O>
    where
        I: Serialize + std::fmt::Debug,
        O: DeserializeOwned + std::fmt::Debug,
    {
        let payload = ExternIO::encode(payload)?;

        let zome_call_unsigned = ZomeCallUnsigned {
            cell_id: cell_id.clone(),
            zome_name: zome_name.clone(),
            fn_name: fn_name.clone(),
            payload,
            cap_secret: None,
            provenance: cell_id.agent_pubkey().clone(),
            nonce: fresh_nonce()?,
            expires_at: (Timestamp::now() + std::time::Duration::from_millis(5000))?,
        };

        let signature = self
            .keystore
            .sign(
                cell_id.agent_pubkey().clone(),
                zome_call_unsigned.data_to_sign()?,
            )
            .await?;

        let zome_call = ZomeCall {
            cell_id,
            zome_name,
            fn_name,
            payload: zome_call_unsigned.payload,
            cap_secret: None,
            provenance: zome_call_unsigned.provenance,
            signature,
            nonce: zome_call_unsigned.nonce,
            expires_at: zome_call_unsigned.expires_at,
        };

        let response = self
            .tx
            .request(AppRequest::CallZome(Box::new(zome_call)))
            .await
            .context("Failed to call zome")?;

        match response {
            AppResponse::ZomeCalled(output) => Ok(output.decode()?),
            _ => Err(anyhow!("Unexpected response")),
        }
    }
}

fn fresh_nonce() -> Result<Nonce256Bits> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes)?;
    Ok(Nonce256Bits::from(bytes))
}
