# Standalone GNU Make entry points; no sibling checkout is required
PROJECT := $(shell makefile='$(subst ','"'"',$(MAKEFILE_LIST))'; makefile=$$(printf '%s' "$$makefile" | sed 's/^ //'); cd "$$(dirname "$$makefile")" && pwd)
PYTHON ?= python3
export WORKSPACE_CONFIG
export WORKSPACE_JOBS
export TARGET
export PRODUCT
# macOS release architecture; both produces separate ARM64 and Intel installers
ARCH ?= both
export ARCH
TARGETS := help deps build build-cli build-frontend package package-target check check-syntax test format format-check desktop dev dev-web
.PHONY: $(TARGETS)
$(TARGETS):
	@$(PYTHON) "$(PROJECT)/scripts/build.py" "$@"

# Linux release packages from macOS, Windows or Linux via the selected Docker builder
.PHONY: build-linux
build-linux:
	@TARGET=linux-amd64 PRODUCT="$${PRODUCT:-all}" $(PYTHON) "$(PROJECT)/scripts/build.py" package-target

# macOS release installers. Explicit aliases ignore ARCH and select only their named target
.PHONY: build-macos build-macos-arm64 build-macos-amd64
build-macos-arm64: MACOS_TARGETS := macos-arm64
build-macos-amd64: MACOS_TARGETS := macos-amd64
build-macos build-macos-arm64 build-macos-amd64:
	@set -e; targets="$(MACOS_TARGETS)"; \
	if [ "$@" = build-macos ]; then \
		case "$$ARCH" in \
			arm64|amd64) targets="macos-$$ARCH" ;; \
			both) targets="macos-arm64 macos-amd64" ;; \
			*) echo "Use ARCH=arm64|amd64|both for build-macos" >&2; exit 2 ;; \
		esac; \
	fi; \
	for target in $$targets; do \
		TARGET="$$target" PRODUCT="$${PRODUCT:-all}" $(PYTHON) "$(PROJECT)/scripts/build.py" package-target; \
	done
