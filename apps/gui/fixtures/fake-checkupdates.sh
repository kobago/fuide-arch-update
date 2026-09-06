#!/bin/bash
# fake-checkupdates.sh: FAKE_UPDATES=n pending repo updates (default 2); 0 exits 2 like the real one.
n="${FAKE_UPDATES:-2}"
state="${FUIDE_ARCH_STATE_DIR:-/tmp/fuide-arch-update-fake}"
# after a fake -Syu nothing is pending any more
[ -f "${state}/fake-upgraded" ] && n=0
if [ "${n}" -eq 0 ]; then
	exit 2
fi
names=(bash ripgrep linux-cachyos mesa systemd)
for ((i = 0; i < n; i++)); do
	echo "${names[i % ${#names[@]}]} 1.${i}.0-1 -> 1.${i}.1-1"
done
exit 0
