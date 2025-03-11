# holochain_env_setup

Test utilities for setting up Holochain environments with conductor and lair-keystore.

## Overview

This crate provides utilities for setting up a complete Holochain environment for testing purposes. It handles:

- Setting up temporary directories
- Starting a Lair keystore instance
- Configuring and starting a Holochain conductor
- Managing the lifecycle of these processes

## Usage

```rust
use holochain_env_setup::environment::setup_environment;
use tempfile::tempdir;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create temporary directories
    let tmp_dir = tempdir()?.into_path();
    let log_dir = tmp_dir.join("log");
    std::fs::create_dir_all(&log_dir)?;

    // Setup the environment
    let env = setup_environment(&tmp_dir, &log_dir, None, None).await?;

    // The environment is now ready with:
    // - A running Holochain conductor on port 4444
    // - A running Lair keystore
    // - All necessary configuration

    // Use the environment...
    let _agent_key = env.keystore.new_sign_keypair_random().await?;

    Ok(())
}
```

## Prerequisites

This crate requires:

- Holochain conductor binary in PATH
- lair-keystore binary in PATH
- Nix environment (recommended)

## Features

- Temporary environment setup for testing
- Automatic process cleanup
- Configurable ports and settings
- Integration with Lair keystore
- Logging support

## License

This project is licensed under the GNU General Public License v3.0 (GPL-3.0). This means:

- You can freely use, modify, and distribute this software
- If you distribute modified versions, you must:
  - Make the source code available
  - License it under GPL-3.0
  - State your modifications
- No warranty is provided

For more details, see the [LICENSE](LICENSE) file or visit https://www.gnu.org/licenses/gpl-3.0.html
