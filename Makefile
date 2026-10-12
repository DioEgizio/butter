# Build and install Butter from source.
#
#   make
#   sudo make install            # into /usr/local
#   make install PREFIX=~/.local # for one user

PREFIX ?= /usr/local
DESTDIR ?=
APP_ID := io.github.microlift.butter
CARGO ?= cargo

BINDIR := $(DESTDIR)$(PREFIX)/bin
DATADIR := $(DESTDIR)$(PREFIX)/share

.PHONY: all check install uninstall clean

all:
	$(CARGO) build --release --locked

check:
	$(CARGO) test --locked
	# -desktop-file-validate data/$(APP_ID).desktop
	# -appstreamcli validate --no-net data/$(APP_ID).metainfo.xml

install:
	install -Dm755 target/release/butter $(BINDIR)/butter
	install -Dm644 data/$(APP_ID).desktop $(DATADIR)/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml $(DATADIR)/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg \
		$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg \
		$(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg

uninstall:
	rm -f $(BINDIR)/butter \
		$(DATADIR)/applications/$(APP_ID).desktop \
		$(DATADIR)/metainfo/$(APP_ID).metainfo.xml \
		$(DATADIR)/icons/hicolor/scalable/apps/$(APP_ID).svg \
		$(DATADIR)/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg

clean:
	$(CARGO) clean
