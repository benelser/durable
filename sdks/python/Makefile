# Build, test, and package the durable-runtime Python SDK
.PHONY: build test test-unit test-e2e bundle clean install

# Build the Rust binary
build:
	cd ../.. && cargo build --release

# Run unit tests (no binary needed)
test-unit:
	python3 -m pytest tests/test_sdk.py -v

# Run end-to-end tests (requires binary)
test-e2e: build
	DURABLE_RUNTIME_PATH=../../target/release/durable-runtime python3 tests/run_e2e.py

# Run all tests
test: test-unit test-e2e

# Bundle the binary into the package
bundle: build
	python3 scripts/bundle_binary.py

# Install locally (editable, with bundled binary)
install: bundle
	pip install -e .

# Clean up
clean:
	rm -rf durable/_bin build dist *.egg-info
	cd ../.. && cargo clean
