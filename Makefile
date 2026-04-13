.PHONY: build install clean

BINARY := openab
TARGET := target/release/$(BINARY)
INSTALL_DIR := $(HOME)/.local/bin

build:
	cargo build --release
	@# macOS 26+ AMFI: adhoc-signed binaries from cargo hang at _dyld_start.
	@# Re-signing fixes this. Safe no-op on Linux.
	@if [ "$$(uname)" = "Darwin" ]; then \
		codesign --force --sign - $(TARGET) && \
		echo "✓ codesigned $(TARGET)"; \
	fi

install: build
	cp $(TARGET) $(INSTALL_DIR)/$(BINARY)
	@if [ "$$(uname)" = "Darwin" ]; then \
		codesign --force --sign - $(INSTALL_DIR)/$(BINARY) && \
		echo "✓ installed + codesigned → $(INSTALL_DIR)/$(BINARY)"; \
	fi

clean:
	cargo clean
