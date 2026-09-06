PREFIX ?= /usr/local
gui = fuide-arch-update
tray = fuide-arch-update-tray

.PHONY: build test install uninstall clean enable-tray disable-tray

build:
	# Build the GUI and the systray applet
	cargo build --release

test:
	cargo test

install:
	install -Dm 755 "target/release/${gui}" "${DESTDIR}${PREFIX}/bin/${gui}"
	install -Dm 755 "target/release/${tray}" "${DESTDIR}${PREFIX}/bin/${tray}"
	install -Dm 644 "res/${gui}.desktop" "${DESTDIR}${PREFIX}/share/applications/${gui}.desktop"
	install -Dm 644 "res/${tray}.desktop" "${DESTDIR}${PREFIX}/share/applications/${tray}.desktop"
	install -Dm 644 "res/${gui}.svg" "${DESTDIR}${PREFIX}/share/icons/hicolor/scalable/apps/${gui}.svg"
	install -Dm 644 README.md "${DESTDIR}${PREFIX}/share/doc/${gui}/README.md"

uninstall:
	rm -f "${DESTDIR}${PREFIX}/bin/${gui}" "${DESTDIR}${PREFIX}/bin/${tray}"
	rm -f "${DESTDIR}${PREFIX}/share/applications/${gui}.desktop" "${DESTDIR}${PREFIX}/share/applications/${tray}.desktop"
	rm -f "${DESTDIR}${PREFIX}/share/icons/hicolor/scalable/apps/${gui}.svg"
	rm -rf "${DESTDIR}${PREFIX}/share/doc/${gui}/"

clean:
	rm -rf target/

# Per-user: start the tray applet with the desktop session (XDG autostart).
enable-tray:
	install -Dm 644 "res/${tray}.desktop" "$${XDG_CONFIG_HOME:-$${HOME}/.config}/autostart/${tray}.desktop"

disable-tray:
	rm -f "$${XDG_CONFIG_HOME:-$${HOME}/.config}/autostart/${tray}.desktop"
