verifiers: fmt check clippy

fmt:
	cargo fmt

check:
	cargo check

clippy:
	cargo clippy

test: verifiers
	cargo test

build: 
	rustup target add x86_64-unknown-linux-musl
	cargo build --release --target=x86_64-unknown-linux-musl

install: build
	sudo mv ./target/x86_64-unknown-linux-musl/release/docker2fl /usr/bin/

coverage:  
	cargo install cargo-tarpaulin
	cargo tarpaulin

