# Builds fsfork, confine and fsdiff, and installs them and their man pages
# under PREFIX:
#
#   make install                     # in ~/.local
#   make install PREFIX=/usr/local   # system-wide, which may need sudo
#
# `make` builds as you, so `make && sudo make install PREFIX=/usr/local`
# builds nothing as root.

PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
MANDIR ?= $(PREFIX)/share/man

COMMANDS = fsfork confine fsdiff
BINARIES = $(COMMANDS:%=target/release/%)
SOURCES = Cargo.toml Cargo.lock $(wildcard fsfork/Cargo.toml fsfork/*.rs cmd/*/Cargo.toml cmd/*/*.rs)

all: $(BINARIES)

$(BINARIES): $(SOURCES)
	cargo build --release
	@touch $(BINARIES)

install: all
	install -d $(DESTDIR)$(BINDIR) $(DESTDIR)$(MANDIR)/man1
	for command in $(COMMANDS); do \
		install -m 755 target/release/$$command $(DESTDIR)$(BINDIR)/$$command && \
		install -m 644 cmd/$$command/$$command.1 $(DESTDIR)$(MANDIR)/man1/$$command.1 || exit 1; \
	done

uninstall:
	for command in $(COMMANDS); do \
		rm -f $(DESTDIR)$(BINDIR)/$$command $(DESTDIR)$(MANDIR)/man1/$$command.1; \
	done

test:
	cargo test

clean:
	cargo clean

.PHONY: all install uninstall test clean
