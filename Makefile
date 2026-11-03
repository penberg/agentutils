# Builds fsfork, confine and fsdiff, and installs them and their man pages
# under PREFIX:
#
#   make install                     # in ~/.local
#   make install PREFIX=/usr/local   # system-wide, which may need sudo
#
# `make` builds as you, so `make && sudo make install PREFIX=/usr/local`
# builds nothing as root.
#
# Each step prints one line such as `  INSTALL ~/.local/bin/fsfork`.
# `make V=1` prints the commands instead.

PREFIX ?= $(HOME)/.local
BINDIR ?= $(PREFIX)/bin
MANDIR ?= $(PREFIX)/share/man

COMMANDS = fsfork confine fsdiff
BINARIES = $(COMMANDS:%=target/release/%)
SOURCES = Cargo.toml Cargo.lock $(wildcard fsfork/Cargo.toml fsfork/*.rs cmd/*/Cargo.toml cmd/*/*.rs)

# $(Q) hides a command, and $(call msg,VERB,what) announces it. With V=1
# the command is shown instead; with `make -s` nothing is.
ifeq ($(V),1)
  Q =
  msg =
else
  Q = @
  msg = @printf '  %-7s %s\n' '$(1)' '$(2)'
endif
ifneq ($(findstring s,$(firstword -$(MAKEFLAGS))),)
  msg =
endif

# The recipe lines that install, or remove, one command and its man page.
define install_command
	$(call msg,INSTALL,$(DESTDIR)$(BINDIR)/$(1))
	$(Q)install -m 755 target/release/$(1) $(DESTDIR)$(BINDIR)/$(1)
	$(call msg,INSTALL,$(DESTDIR)$(MANDIR)/man1/$(1).1)
	$(Q)install -m 644 cmd/$(1)/$(1).1 $(DESTDIR)$(MANDIR)/man1/$(1).1

endef
define uninstall_command
	$(call msg,RM,$(DESTDIR)$(BINDIR)/$(1))
	$(Q)rm -f $(DESTDIR)$(BINDIR)/$(1)
	$(call msg,RM,$(DESTDIR)$(MANDIR)/man1/$(1).1)
	$(Q)rm -f $(DESTDIR)$(MANDIR)/man1/$(1).1

endef

all: $(BINARIES)

$(BINARIES): $(SOURCES)
	$(call msg,CARGO,target/release)
	$(Q)cargo build --release
	$(Q)touch $(BINARIES)

install: all
	$(Q)install -d $(DESTDIR)$(BINDIR) $(DESTDIR)$(MANDIR)/man1
	$(foreach command,$(COMMANDS),$(call install_command,$(command)))

uninstall:
	$(foreach command,$(COMMANDS),$(call uninstall_command,$(command)))

test:
	$(call msg,TEST,workspace)
	$(Q)cargo test

clean:
	$(call msg,CLEAN,target)
	$(Q)cargo clean

.PHONY: all install uninstall test clean
