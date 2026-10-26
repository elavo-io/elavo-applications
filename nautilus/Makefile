# The Applications folder's Nautilus module. Nautilus 50 loads every shared
# library in <libdir>/nautilus/extensions-4. Build against the system GTK 4;
# no Nautilus headers are needed.
CC ?= gcc
CFLAGS ?= -O2 -Wall -Wextra -Wno-missing-field-initializers
PKG_CONFIG ?= pkg-config

libelavo-applications.so: elavo-applications-nautilus.c
	$(CC) -shared -fPIC $(CFLAGS) -o $@ elavo-applications-nautilus.c \
		$$($(PKG_CONFIG) --cflags --libs gtk4 gio-unix-2.0) -ldl

clean:
	rm -f libelavo-applications.so

.PHONY: clean
