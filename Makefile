# ham — the gates. There is no CI on this repo, so these are the whole check.

.DEFAULT_GOAL := help

help: ## Show this help
	@grep -hE '^[a-zA-Z0-9_-]+:.*?## .*$$' $(firstword $(MAKEFILE_LIST)) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

build: ## Build the crate
	cargo build --release

fmt: ## Format
	cargo fmt

fmt-check: ## Check formatting
	cargo fmt --check

test: ## Run the tests
	cargo test

check: fmt-check test ## Everything a change must pass before it ships

.PHONY: help build fmt fmt-check test check
