# Holochain App Manager (HAM)

## Overview

HAM is a command-line utility and Rust library for managing Holochain applications. It simplifies the process of installing and enabling Holochain apps (.happ files) on a running conductor.

## Prerequisites

- [Nix](https://nixos.org/download.html) package manager
- A running Holochain conductor

## Installation

Clone this repository and build the project:

```bash
git clone https://github.com/unytco/ham.git
cd ham
nix develop
cargo build --release
```

## Usage

### Command Line Interface

Basic usage:

```bash
ham --happ path/to/your/app.happ
```

All available options:

```bash
ham --port 4444 --happ path/to/your/app.happ --network-seed "optional-network-seed"

Options:

- `-p, --port <PORT>`: The admin port of your Holochain conductor (default: 4444)
- `-h, --happ <PATH>`: Path to the .happ file you want to install (required)
- `-n, --network-seed <SEED>`: Optional network seed for the app

```

## Development

### Running Tests

Tests must be run within the Nix environment as they depend on Holochain and Lair binaries:

bash
nix develop
cargo test --all

### Project Structure

- `crates/ham/`: The main HAM library and CLI implementation
- `crates/holochain_env_setup/`: Testing utilities for Holochain environment setup

## License

Copyright (C) 2024, Unyt.co

This program is free software: you can redistribute it and/or modify it under the terms of the license provided in the LICENSE file. This program is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

### Testing

The test suite requires running inside the Nix environment to ensure all dependencies (Holochain, Lair) are available. GitHub Actions is configured to run all tests in the correct environment automatically for all PRs and pushes to main.
