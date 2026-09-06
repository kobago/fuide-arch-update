#!/bin/bash
# fake-pacman.sh: stands in for `pacman` in tests / screenshots (FUIDE_ARCH_PACMAN).
# Read-only queries answer from fixture files; mutations print what pacman prints and ask
# its questions without touching the system. State: $FUIDE_ARCH_STATE_DIR/fake-installed
# (names removed / added by -Rns / -S) so the inventory reflects what happened.
#   FAKE_FAIL=1     -S / -Syu fail after the confirmation (exit 1)
#   FAKE_SLOW=1     pause between steps
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
state="${FUIDE_ARCH_STATE_DIR:-/tmp/fuide-arch-update-fake}"
mkdir -p "${state}"
removed="${state}/fake-removed"
added="${state}/fake-added"
marked="${state}/fake-marked"
touch "${removed}" "${added}" "${marked}"
pause() { [ -n "${FAKE_SLOW}" ] && sleep "${1:-0.5}"; return 0; }
bold="\e[1m"; blue="${bold}\e[34m"; off="\e[0m"

# strip --color never and the like
args=()
while [ $# -gt 0 ]; do
	case "$1" in
		--color) shift ;;
		never|always|auto) ;;
		*) args+=("$1") ;;
	esac
	shift
done
set -- "${args[@]}"
op="$1"; shift

is_removed() { grep -qx "$1" "${removed}"; }

case "${op}" in
	-Qi)
		# every block of qi.txt whose Name is not removed
		awk -v removed="${removed}" '
			BEGIN { while ((getline l < removed) > 0) gone[l] = 1; RS = ""; ORS = "\n\n" }
			{ match($0, /Name            : [^\n]*/); name = substr($0, RSTART + 18, RLENGTH - 18); if (!(name in gone)) print $0 }
		' "${here}/qi.txt"
		# packages installed through the fake -S
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
			printf 'Repository      : extra\nName            : %s\nVersion         : 15.2.0-1\nDescription     : Details for %s from the fake -Si\nURL             : https://example.org/%s\nLicenses        : MIT\nDepends On      : glibc  pcre2\nDownload Size   : 1330.50 KiB\nInstalled Size  : 3654.88 KiB\n\n' "${name}" "${name}" "${name}"
		done
		;;
	-S|-Syu)
		pause 1
		if [ "${op}" = "-Syu" ]; then
			echo ":: Synchronizing package databases..."
			printf ' core downloading...\r core is up to date\n'
			echo ":: Starting full system upgrade..."
		fi
		names=()
		for a in "$@"; do case "$a" in --*) ;; *) names+=("$a") ;; esac; done
		echo "resolving dependencies..."
		echo "looking for conflicting packages..."
		echo
		printf "Packages (%d) %s\n\n" "${#names[@]}" "${names[*]}"
		echo "Total Installed Size:  3.57 MiB"
		echo
		read -rp ":: Proceed with installation? [Y/n] " answer
		case "${answer}" in
			Y|y|"") ;;
			*) echo "aborted"; exit 1 ;;
		esac
		if [ -n "${FAKE_FAIL}" ]; then
			echo "error: failed to commit transaction (conflicting files)"
			echo "Errors occurred, no packages were upgraded."
			exit 1
		fi
		for n in "${names[@]}"; do
			printf '(1/1) installing %s\r(1/1) installing %s   [########] 100%%\n' "$n" "$n"
			grep -qx "$n" "${added}" || echo "$n" >> "${added}"
			sed -i "/^$n\$/d" "${removed}"
		done
		[ "${op}" = "-Syu" ] && : > "${state}/fake-upgraded"
		echo ":: Running post-transaction hooks..."
		exit 0
		;;
	-Rns)
		names=("$@")
		echo "checking dependencies..."
		echo
		printf "Packages (%d) %s\n\n" "${#names[@]}" "${names[*]}"
		read -rp ":: Do you want to remove these packages? [Y/n] " answer
		case "${answer}" in
			Y|y|"") ;;
			*) echo "aborted"; exit 1 ;;
		esac
		for n in "${names[@]}"; do
			echo "(1/1) removing $n"
			echo "$n" >> "${removed}"
		done
		exit 0
		;;
	-D)
		echo "$*" >> "${marked}"
		echo "${2}: install reason has been set to '${1#--as}'"
		exit 0
		;;
	*)
		echo "fake-pacman: unhandled ${op} $*" >&2
		exit 1
		;;
esac
