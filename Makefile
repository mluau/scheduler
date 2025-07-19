test:
	cargo build
	cargo build --features send
release:
	cargo build --release
