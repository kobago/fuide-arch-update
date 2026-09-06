#!/bin/bash
# fake-sudo.sh: asks a password like sudo (on the tty, no echo) then runs the command as-is.
# `wrong` gets "Sorry, try again." and a second prompt, like sudo.
state="${FUIDE_ARCH_STATE_DIR:-/tmp/fuide-arch-update-fake}"
mkdir -p "${state}"
read -rsp "[sudo] password for ${USER:-user}: " pw < /dev/tty
echo
if [ "${pw}" = "wrong" ]; then
	echo "Sorry, try again."
	read -rsp "[sudo] password for ${USER:-user}: " pw < /dev/tty
	echo
fi
echo "${pw}" > "${state}/fake-password-seen"
exec "$@"
