PREFIX ?= /usr/local
BINDIR ?= $(PREFIX)/bin
SHAREDIR ?= $(PREFIX)/share
DESTDIR ?=
CARGO ?= cargo

.PHONY: all build install install-files uninstall test clean

all: build

build:
	$(CARGO) build --release

install: build install-files

# Install the already-built binary and desktop integration files.
# Useful when cargo is only available for the unprivileged user.
install-files:
	PREFIX="$(PREFIX)" DESTDIR="$(DESTDIR)" ./install.sh

uninstall:
	rm -f "$(DESTDIR)$(BINDIR)/red"
	rm -f "$(DESTDIR)$(SHAREDIR)/applications/red.desktop"
	rm -f "$(DESTDIR)$(SHAREDIR)/mime/packages/red.xml"
	rm -f "$(DESTDIR)$(SHAREDIR)/icons/hicolor/scalable/apps/red.svg"
	rm -f "$(DESTDIR)$(SHAREDIR)/kio/servicemenus/red-servicemenu.desktop"
	@if [ -z "$(DESTDIR)" ]; then \
		command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$(SHAREDIR)/applications" 2>/dev/null || true; \
		command -v update-mime-database >/dev/null 2>&1 && update-mime-database "$(SHAREDIR)/mime" 2>/dev/null || true; \
	fi

test:
	$(CARGO) test

clean:
	$(CARGO) clean
