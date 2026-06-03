# marv — developer task runner. Thin wrappers over cargo; the toolchain is pinned
# in rust-toolchain.toml so fmt/clippy are deterministic.

CARGO ?= cargo
MARV  ?= $(CARGO) run -q -p marv-cli --

.DEFAULT_GOAL := help

.PHONY: help build release test fmt fmt-check clippy check-all verify demo wasm-demo clean

help: ## List targets
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | \
	  awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

build: ## Debug build of the whole workspace
	$(CARGO) build --workspace

release: ## Optimized build (binary at target/release/marv)
	$(CARGO) build --release

test: ## Run the full test suite (z3-backed verify tests run if z3 is on PATH)
	$(CARGO) test --workspace

fmt: ## Format the workspace
	$(CARGO) fmt --all

fmt-check: ## Check formatting (CI gate)
	$(CARGO) fmt --all --check

clippy: ## Lint with warnings denied (CI gate)
	$(CARGO) clippy --workspace --all-targets -- -D warnings

check-all: fmt-check clippy test ## Everything CI runs

verify: ## Tier-2 verify the clamp example (needs z3)
	$(MARV) verify examples/clamp.mv

demo: release ## Run the interpreter↔Cranelift demo on factorial
	@echo "interpreter:"; $(MARV) run examples/factorial.mv --entry factorial 10
	@echo "cranelift:  "; $(MARV) build --run examples/factorial.mv --entry factorial 10

wasm-demo: release ## Build the browser-sandbox .wasm artifacts
	$(MARV) build --target wasm-component examples/factorial.mv -o web/factorial.wasm
	$(MARV) build --target wasm-component examples/arithmetic.mv -o web/arithmetic.wasm
	$(MARV) build --target wasm-component web/fetcher.core.json -o web/fetcher.wasm
	@echo "Serve: (cd web && python3 -m http.server 8087)"

clean: ## Remove build artifacts
	$(CARGO) clean
