use super::lair;
use crate::holochain::spawn_holochain;
use crate::taskgroup_manager::kill_on_drop::KillChildOnDrop;
use anyhow::Context;
use holochain_keystore::MetaLairClient;
use lair_keystore_api::prelude::LairServerConfigInner as LairConfig;
use snafu::Snafu;
use std::path::{Path, PathBuf};
use std::{fs, time::Duration};
use tokio::time::sleep;
use tracing::info;

async fn wait_for_conductor(log_path: &Path) -> anyhow::Result<()> {
    for i in 1..=30 {
        if let Ok(logs) = fs::read_to_string(log_path) {
            if logs.contains("Conductor successfully initialized") {
                info!("Conductor initialization confirmed");
                return Ok(());
            }
        }
        info!("Waiting for conductor initialization... (attempt {})", i);
        sleep(Duration::from_secs(1)).await;
    }
    Err(anyhow::anyhow!(
        "Conductor failed to initialize after 30 seconds"
    ))
}

pub async fn setup_environment(
    tmp_dir: &Path,
    log_dir: &Path,
    device_bundle: Option<&str>,
    lair_fallback: Option<(PathBuf, u16)>,
) -> Result<Environment, SetupEnvironmentError> {
    info!("Starting lair-keystore");
    let (lair, lair_config, keystore) = lair::spawn(tmp_dir, log_dir, device_bundle, lair_fallback)
        .await
        .unwrap();

    info!("Starting Holochain conductor");
    let holochain = spawn_holochain(tmp_dir, log_dir, lair_config.clone());

    // Wait for conductor to be ready
    wait_for_conductor(&log_dir.join("holochain.txt"))
        .await
        .context("Failed waiting for conductor")?;

    Ok(Environment {
        _holochain: holochain,
        _lair: lair,
        lair_config,
        keystore,
    })
}
#[derive(Debug, Snafu)]
pub enum SetupEnvironmentError {
    AdminWs { source: anyhow::Error },
    AppWs { source: anyhow::Error },
    LairClient { source: one_err::OneErr },
    ZomeCallSigning { source: one_err::OneErr },
    Anyhow { source: anyhow::Error },
    AppBundleE { source: anyhow::Error },
    FfsIo { source: anyhow::Error },
}

impl From<anyhow::Error> for SetupEnvironmentError {
    fn from(err: anyhow::Error) -> Self {
        SetupEnvironmentError::Anyhow { source: err }
    }
}

pub struct Environment {
    _holochain: KillChildOnDrop,
    _lair: KillChildOnDrop,
    pub lair_config: LairConfig,
    pub keystore: MetaLairClient,
}
