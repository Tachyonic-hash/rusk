help: ## Display this help screen
	@grep -h -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

wasm: ## Generate the WASM for the contract given (e.g. make wasm for=transfer)
	@cargo rustc \
		--manifest-path=contracts/$(for)/Cargo.toml \
		--release \
		--target wasm32-unknown-unknown \
		-- -C link-args=-s

contracts: ## Generate the WASM for all the contracts & test them
	$(MAKE) -C ./contracts/transfer/
	$(MAKE) -C ./contracts/bid/

circuits: ## Build and test circuit crates
	cd circuits/bid && cargo test --release
	cd circuits/transfer && cargo test --release

test: ## Run the tests
	cargo test --release -- --nocapture  && \
	@make contracts 
	
run: ## Run the server
		@make contracts && \
			cargo run --release

.PHONY: help wasm contracts test run
