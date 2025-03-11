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
# Or install from URL
ham --happ https://example.com/path/to/app.happ
```

Install with network seed:

```bash
ham --happ path/to/your/app.happ --network-seed 1234567890
```

```bash
ham --happ https://example.com/path/to/app.happ --network-seed 1234567890
```

## License

This project is licensed under the GNU General Public License v3.0 (GPL-3.0).
See the [LICENSE](LICENSE) file for details.
