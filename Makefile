DSTDIR ?= /usr/local
BINDIR := $(DSTDIR)/bin
TARGET := target/release/clevo-indicator-rs
INSTALLED := $(BINDIR)/clevo-indicator-rs

.PHONY: all install test uninstall clean

all:
	cargo build --release

install: all
	@echo "Install to $(INSTALLED)"
	@sudo install -d -m 755 $(BINDIR)
	@sudo install -m 4750 -g adm $(TARGET) $(INSTALLED)

test:
	cargo test

uninstall:
	@echo "Remove $(INSTALLED)"
	@sudo rm -f $(INSTALLED)

clean:
	cargo clean
