#!/bin/bash
# fake-yay.sh: stands in for the AUR helper in tests / screenshots (FUIDE_ARCH_AUR_HELPER).
# -Qua answers from $FAKE_AUR_UPDATES (default: one), -Ss --aur / -Si --aur are canned, -S /
# -Syu ask a sudo password (like yay calling sudo) and the pacman question.
state="${FUIDE_ARCH_STATE_DIR:-/tmp/fuide-arch-update-fake}"
mkdir -p "${state}"
pause() { [ -n "${FAKE_SLOW}" ] && sleep "${1:-0.5}"; return 0; }

args=()
aur_only=""
while [ $# -gt 0 ]; do
	case "$1" in
		--color) shift ;;
		never|always|auto) ;;
		--aur|-a) aur_only=1 ;;
		*) args+=("$1") ;;
	esac
	shift
done
set -- "${args[@]}"
op="$1"; shift

case "${op}" in
	-Qua)
		if [ "${FAKE_AUR_UPDATES:-1}" -gt 0 ]; then
			echo "visual-studio-code-bin 1.136.0-1 -> 1.136.1-1"
		fi
		exit 0
		;;
	-Ss)
		q="$1"
		case "${q}" in
			ripgrep|rip) printf 'aur/ripgrep-git 15.2.0.r3-1 (+9 0.10) [12d3h]\n    Search tool, git version\n' ;;
			pikaur) printf 'aur/pikaur 1.33.3-1 (+300 4.21) [1d]\n    AUR helper which asks all questions before installing\n' ;;
			*) exit 1 ;;
		esac
		;;
	-Si)
		for name in "$@"; do
			printf 'Repository                    : aur\nName                          : %s\nVersion                       : 1.0-1\nDescription                   : AUR details for %s\nURL                           : https://aur.example.org/%s\nLicenses                      : MIT\nDepends On                    : glibc\n                                gcc-libs\nAUR URL                       : https://aur.archlinux.org/packages/%s\nMaintainer                    : someone\nPopularity                    : 4.21\nVotes                         : 300\nOut-of-date                   : No\n\n' "${name}" "${name}" "${name}" "${name}"
		done
		;;
	-S|-Syu)
		names=()
		for a in "$@"; do case "$a" in --*) ;; *) names+=("$a") ;; esac; done
		if [ "${op}" = "-Syu" ]; then
			echo ":: Synchronizing package databases..."
			echo ":: Starting full system upgrade..."
		fi
		echo "==> Making package: ${names[0]:-all} (fake)"
		read -rp "==> Diffs to show? [N]one [A]ll [Ab]ort [I]nstalled [No]tInstalled or (1 2 3, 1-3, ^4) " diffs
		echo "==> diffs=${diffs}" >> "${state}/fake-yay-log"
		# yay calls sudo itself
		read -rsp "[sudo] password for ${USER:-user}: " pw
		echo
		echo "${pw}" > "${state}/fake-password-seen"
		pause 1
		read -rp ":: Proceed with installation? [Y/n] " answer
		case "${answer}" in
			Y|y|"") ;;
			*) echo "aborted"; exit 1 ;;
		esac
		for n in "${names[@]}"; do
			echo "(1/1) installing $n"
			echo "$n" >> "${state}/fake-added"
		done
		[ "${op}" = "-Syu" ] && : > "${state}/fake-upgraded"
		exit 0
		;;
	*)
		echo "fake-yay: unhandled ${op} $*" >&2
		exit 1
		;;
esac
