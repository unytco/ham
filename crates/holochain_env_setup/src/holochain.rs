use crate::taskgroup_manager::kill_on_drop::{kill_on_drop, KillChildOnDrop};
use lair_keystore_api::prelude::LairServerConfigInner as LairConfig;
use snafu::Snafu;
use std::{
    fs::File,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::TempDir;
use tracing::{info, trace};
use url2::Url2;

pub fn default_password() -> String {
    std::env::var("HOLOCHAIN_DEFAULT_PASSWORD").unwrap_or_else(|_| "super-secret".to_string())
}

pub fn spawn_holochain(
    tmp_dir: &Path,
    logs_dir: &Path,
    lair_config: LairConfig,
) -> KillChildOnDrop {
    let lair_connection_url = lair_config.connection_url.to_string();

    let admin_port = 4444;

    let holochain_config_name = "holochain-config.yaml";
    write_holochain_config(
        &tmp_dir.join(holochain_config_name),
        lair_connection_url,
        admin_port,
    )
    .unwrap();

    // spin up holochain
    let mut holochain = kill_on_drop(
        Command::new("holochain")
            .current_dir(tmp_dir)
            .arg("--config-path")
            .arg("holochain-config.yaml")
            .arg("--piped")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(logs_dir.join("holochain.txt")).unwrap())
            .spawn()
            .unwrap(),
    );

    {
        let mut holochain_input = holochain.stdin.take().unwrap();
        let passphrase = default_password();
        holochain_input.write_all(passphrase.as_bytes()).unwrap();
    }

    for line in std::io::BufReader::new(holochain.stdout.as_mut().unwrap()).lines() {
        let line = line.unwrap();
        trace!("{:?}", line);
        if line == "Conductor ready." {
            eprintln!("Encountered magic string");
            break;
        }
    }

    holochain
}

pub fn create_tmp_dir() -> PathBuf {
    TempDir::new().unwrap().into_path()
}

pub fn create_log_dir() -> PathBuf {
    TempDir::new().unwrap().into_path()
}

#[derive(Debug, Snafu)]
pub enum WriteHolochainConfigError {
    CreateHolochainConfig { path: PathBuf },
}

fn write_holochain_config(
    path: &Path,
    lair_connection_url: String,
    admin_port: u16,
) -> anyhow::Result<()> {
    use holochain_conductor_api::{
        conductor::paths::DataRootPath, conductor::ConductorConfig,
        config::conductor::KeystoreConfig, config::AdminInterfaceConfig,
    };

    let config = ConductorConfig {
        data_root_path: Some(DataRootPath::from(path.parent().unwrap().join("data"))),
        keystore: KeystoreConfig::LairServer {
            connection_url: Url2::parse(&lair_connection_url),
        },
        admin_interfaces: Some(vec![AdminInterfaceConfig {
            driver: holochain_conductor_api::config::InterfaceDriver::Websocket {
                port: admin_port,
                allowed_origins: holochain_types::websocket::AllowedOrigins::Any,
            },
        }]),
        network: Default::default(),
        db_sync_strategy: Default::default(),
        tracing_override: None,
        device_seed_lair_tag: Some("holochain-device-seed".to_string()),
        danger_generate_throwaway_device_seed: false,
        dpki: Default::default(),
        tuning_params: Default::default(),
    };

    std::fs::write(
        path,
        serde_yaml::to_string(&config).expect("Failed to serialize config"),
    )
    .expect("Failed to write config file");

    info!("Wrote conductor config to {:?}", path);
    Ok(())
}
