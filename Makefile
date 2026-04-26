.PHONY: build test cov cov-pure run-haiku run-opus install-cov check

build:
	cargo build --workspace

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
