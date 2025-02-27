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

All available options:

```

```
