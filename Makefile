# Pledge development workflow.
#
#   make build   compile pledge.wasm (< 1 MiB gate included)
#   make test    unit + manifest + grammar tests (no component needed)
#   make e2e     full suite against the real component and real storage
#   make check   fmt + clippy + deny + machete + e2e (the CI gate)
#   make clean   drop build artifacts (keeps the toolchain)

COMPONENT := $(CURDIR)/pledge.wasm
export DEKOPON_PROVIDER_COMPONENT := $(COMPONENT)

.PHONY: build test e2e check clippy fmt clean install uninstall deny machete

build:
	scripts/build-component.sh $(COMPONENT)

test:
	env -u DEKOPON_PROVIDER_COMPONENT cargo test --locked

e2e: build
	cargo test --locked

clippy:
	cargo clippy --locked --all-targets -- -D warnings

fmt:
	cargo fmt --check

deny:
	cargo deny check

machete:
	cargo machete

check: fmt clippy deny machete e2e

clean:
	rm -rf target $(COMPONENT) $(COMPONENT).sha256

# Install the built component into a broker providers directory:
#   make install PROVIDERS_DIR=/path/to/providers
install: build
	@test -n "$(PROVIDERS_DIR)" || (echo "error: set PROVIDERS_DIR" >&2; exit 2)
	mkdir -p "$(PROVIDERS_DIR)"
	cp $(COMPONENT) $(COMPONENT).sha256 "$(PROVIDERS_DIR)/"
	chmod 0644 "$(PROVIDERS_DIR)/pledge.wasm" "$(PROVIDERS_DIR)/pledge.wasm.sha256"

uninstall:
	@test -n "$(PROVIDERS_DIR)" || (echo "error: set PROVIDERS_DIR" >&2; exit 2)
	rm -f "$(PROVIDERS_DIR)/pledge.wasm" "$(PROVIDERS_DIR)/pledge.wasm.sha256"
