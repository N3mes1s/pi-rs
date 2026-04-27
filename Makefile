.PHONY: build build-release test cov cov-pure run-haiku run-opus install-cov check

build:
	cargo build --workspace

# Release artefact tuned for size + cold-start (musl static, no unwind tables).
# Drops the binary at ./target/release/pi for downstream tooling.
build-release:
	cargo build --release -p pi-coding-agent
	@bin=$$(ls target/x86_64-unknown-linux-musl/release/pi 2>/dev/null || echo target/release/pi); \
	  install -m 755 $$bin target/release/pi; \
	  objcopy --remove-section=.eh_frame \
	          --remove-section=.eh_frame_hdr \
	          --remove-section=.gcc_except_table \
	          target/release/pi 2>/dev/null || true; \
	  ls -l target/release/pi

test:
	cargo test --workspace

# Whole-workspace coverage (low because of IO/runtime modules).
cov:
	cargo llvm-cov --workspace --summary-only

# Coverage on the testable, deterministic modules — what >90% targets.
cov-pure:
	bash scripts/coverage.sh

# Smoke-test against the live Anthropic API, haiku for speed.
run-haiku:
	./target/debug/pi --no-context-files --no-session \
		--provider anthropic --model claude-haiku-4-5-20251001 \
		-p "say exactly: pi-rs OK"

run-opus:
	./target/debug/pi --no-context-files --no-session \
		--provider anthropic --model claude-opus-4-7 \
		-p "say exactly: pi-rs+opus OK"

install-cov:
	cargo install cargo-llvm-cov
	rustup component add llvm-tools-preview

check:
	cargo build --workspace
	cargo test --workspace
	bash scripts/coverage.sh
