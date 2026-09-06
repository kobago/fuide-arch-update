#!/bin/bash
# fake-pkexec.sh: stands in for `pkexec` in tests / screenshots (FUIDE_ARCH_PKEXEC).
# The real one shows the desktop's polkit password dialog; this one just records that
# authentication happened and runs the command "as root" (FAKE_PKEXEC=1 for fake-pacman).
#   FAKE_AUTH_FAIL=dismiss   exit 126 like a dismissed dialog
#   FAKE_AUTH_FAIL=deny      exit 127 like a refused authorisation
state="${FUIDE_ARCH_STATE_DIR:-/tmp/fuide-arch-update-fake}"
mkdir -p "${state}"
case "${FAKE_AUTH_FAIL}" in
	dismiss) echo "Error executing command as another user: Request dismissed" >&2; exit 126 ;;
	deny) echo "Error executing command as another user: Not authorized" >&2; exit 127 ;;
esac
echo "$*" >> "${state}/fake-authenticated"
FAKE_PKEXEC=1 exec "$@"
