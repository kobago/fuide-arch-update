#!/bin/bash
# fake-pacman.sh: stands in for `pacman` in tests / screenshots (FUIDE_ARCH_PACMAN).
# Read-only queries answer from fixture files; mutations print what pacman prints without
# touching the system and record what happened under $FUIDE_ARCH_STATE_DIR so the inventory
# reflects it (fake-removed / fake-added / fake-marked / fake-upgraded).
# Mutations refuse to run unless FAKE_PKEXEC=1 (set by fake-pkexec.sh), like pacman refusing
# to run without root.
#   FAKE_FAIL=1     -S / -Syu fail (exit 1)
#   FAKE_SLOW=1     pause between steps
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
state="${FUIDE_ARCH_STATE_DIR:-/tmp/fuide-arch-update-fake}"
mkdir -p "${state}"
removed="${state}/fake-removed"
added="${state}/fake-added"
marked="${state}/fake-marked"
touch "${removed}" "${added}" "${marked}"
pause() { [ -n "${FAKE_SLOW}" ] && sleep "${1:-0.5}"; return 0; }

args=()
noconfirm=""
while [ $# -gt 0 ]; do
	case "$1" in
		--color) shift ;;
		never|always|auto) ;;
		--noconfirm) noconfirm=1 ;;
		--needed) ;;
		*) args+=("$1") ;;
	esac
	shift
done
set -- "${args[@]}"
op="$1"; shift

is_removed() { grep -qx "$1" "${removed}"; }
need_root() {
	if [ -z "${FAKE_PKEXEC}" ]; then
		echo "error: you cannot perform this operation unless you are root." >&2
		exit 1
	fi
	# -D never asks; the transaction operations must be told not to
	[ "$1" = "-D" ] && return 0
	if [ -z "${noconfirm}" ]; then
		echo "error: fake pacman needs --noconfirm (there is no terminal to ask on)" >&2
		exit 1
	fi
}

case "${op}" in
	-Qi)
		awk -v removed="${removed}" '
			BEGIN { while ((getline l < removed) > 0) gone[l] = 1; RS = ""; ORS = "\n\n" }
			{ match($0, /Name            : [^\n]*/); name = substr($0, RSTART + 18, RLENGTH - 18); if (!(name in gone)) print $0 }
		' "${here}/qi.txt"
		while read -r name; do
			[ -z "${name}" ] && continue
			is_removed "${name}" && continue
			printf 'Installed From  : extra\nName            : %s\nVersion         : 1.0-1\nDescription     : installed by the fake\nInstall Reason  : Explicitly installed\nInstalled Size  : 100.00 KiB\n\n' "${name}"
		done < "${added}"
		;;
	-Qmq) echo "yay"; echo "visual-studio-code-bin" ;;
	-Qtdq) is_removed "orphan-lib" || echo "orphan-lib" ;;
	-Qeq) echo "bash"; echo "yay" ;;
	-Sl) printf 'extra ripgrep 15.2.0-1\nextra bash 5.3.15-2 [installed]\n' ;;
	-Ss)
		q="$1"
		case "${q}" in
			ripgrep|rip) printf 'extra/ripgrep 15.2.0-1\n    A search tool that combines the usability of ag with the raw speed of grep\nextra/ripgrep-all 0.10.6-1\n    rga: ripgrep, but also search in PDFs\n' ;;
			bash) printf 'core/bash 5.3.15-2 [installed]\n    The GNU Bourne Again shell\n' ;;
			*) exit 1 ;;
		esac
		;;
	-Si)
		for name in "$@"; do
			printf 'Repository      : extra\nName            : %s\nVersion         : 0.10.6-1\nDescription     : details for %s\nURL             : https://example.org/%s\nLicenses        : AGPL-3.0\nDepends On      : glibc  pcre2\nDownload Size   : 2.00 MiB\nInstalled Size  : 6.00 MiB\n\n' "${name}" "${name}" "${name}"
		done
		;;
	-S)
		need_root
		echo "resolving dependencies..."
		pause
		echo "Packages (1) $1-1.0-1"
		if [ -n "${FAKE_FAIL}" ]; then
			echo "error: failed to commit transaction (conflicting files)" >&2
			exit 1
		fi
		echo ":: Processing package changes..."
		for name in "$@"; do
			echo "installing ${name}..."
			echo "${name}" >> "${added}"
		done
		;;
	-Rns)
		need_root
		echo "checking dependencies..."
		for name in "$@"; do
			echo "removing ${name}..."
			echo "${name}" >> "${removed}"
		done
		;;
	-Syu)
		need_root
		echo ":: Synchronizing package databases..."
		pause
		echo ":: Starting full system upgrade..."
		if [ -n "${FAKE_FAIL}" ]; then
			echo "error: failed retrieving file 'core.db' from mirror : Could not resolve host" >&2
			exit 1
		fi
		echo "Packages (2) bash-5.3.16-1  ripgrep-15.3.0-1"
		echo ":: Processing package changes..."
		touch "${state}/fake-upgraded"
		;;
	-D)
		need_root -D
		echo "$*" >> "${marked}"
		;;
	*)
		echo "fake pacman: unsupported operation ${op}" >&2
		exit 1
		;;
esac
