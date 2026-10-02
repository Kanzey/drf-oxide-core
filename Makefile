PYTHON ?= python3
LIBDIR := $(shell $(PYTHON) -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')

.PHONY: develop release test test-rust test-python lint

develop:
	uv run --no-sync maturin develop --uv

release:
	uv run --no-sync maturin develop --uv --release

test: test-rust test-python

test-rust:
	PYO3_PYTHON=$(PYTHON) LD_LIBRARY_PATH=$(LIBDIR) cargo test --no-default-features

test-python:
	uv run --no-sync pytest

lint:
	cargo fmt --check
	cargo clippy --no-default-features -- -D warnings
