.PHONY: all test build clean

all: clean test build

LEVEL ?= "info"
TEST ?= ""

build-debug: 
	cargo build

build:
	cargo build --release

test: clean-tests
	CARGO_INCREMENTAL=0 RUSTFLAGS='-Cinstrument-coverage' LLVM_PROFILE_FILE='cargo-test-%p-%m.profraw' cargo test  -- --nocapture

test-by-name: clean-tests
	CARGO_INCREMENTAL=0 RUSTFLAGS='-Cinstrument-coverage' LLVM_PROFILE_FILE='cargo-test-%p-%m.profraw' cargo test $(TEST) -- --nocapture

cover:
	grcov . --binary-path ./target/debug/deps/ -s . -t html --branch --ignore-not-existing --ignore '../*' --ignore "/*" --ignore "src/main.rs" --ignore "src/api/*"  -o target/coverage/html
	cp target/coverage/html/html/badges/flat.svg assets/

clean-all:
	rm -rf cargo-test*
	cargo clean

clean-tests:
	rm -rf cargo-test*
