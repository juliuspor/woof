.PHONY: check test build fmt verify

check:
	cargo clippy --workspace --all-targets --locked -- -D warnings
	npm run check

test:
	cargo test --workspace --all-targets --locked
	npm test

build:
	cargo build --workspace --release --locked --target aarch64-apple-darwin
	npm run build

fmt:
	cargo fmt --all

verify:
	scripts/verify.sh
