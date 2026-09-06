#!/bin/bash
# fake-yay.sh: stands in for the AUR helper in tests / screenshots (FUIDE_ARCH_AUR_HELPER).
# -Qua answers from $FAKE_AUR_UPDATES (default: one), -Ss --aur / -Si --aur are canned.
# -S / -Syu require --noconfirm and --sudo <cmd> (what the GUI passes) and call the sudo
# command for the pacman step, like yay does; the run is recorded in fake-yay-log.
state="${FUIDE_ARCH_STATE_DIR:-/tmp/fuide-arch-update-fake}"
mkdir -p "${state}"
pause() { [ -n "${FAKE_SLOW}" ] && sleep "${1:-0.5}"; return 0; }

args=()
aur_only=""
noconfirm=""
sudo_cmd=""
while [ $# -gt 0 ]; do
	case "$1" in
		--color) shift ;;
		never|always|auto) ;;
		--aur|-a) aur_only=1 ;;
		--noconfirm) noconfirm=1 ;;
		--sudo) sudo_cmd="$2"; shift ;;
		*) args+=("$1") ;;
	esac
	shift
done
set -- "${args[@]}"
op="$1"; shift

case "${op}" in
	-Qua)
		if [ "${FAKE_AUR_UPDATES:-1}" -gt 0 ] && [ ! -f "${state}/fake-upgraded" ]; then
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
		if [ -z "${noconfirm}" ] || [ -z "${sudo_cmd}" ]; then
			echo "error: fake yay needs --noconfirm and --sudo <cmd>" >&2
			exit 1
		fi
		echo "op=${op} args=$* sudo=${sudo_cmd}" >> "${state}/fake-yay-log"
		if [ "${op}" = "-Syu" ]; then
			echo ":: Synchronizing package databases..."
			pause
			echo ":: Starting full system upgrade..."
		fi
		echo "==> Making package: ${1:-all} (fake)"
		pause
		echo -e "\e[1;32m==>\e[0m Finished making: ${1:-all} (fake)"
		# the pacman step goes through the privilege command, like the real helper
		FAKE_PKEXEC="" "${sudo_cmd}" true || exit 1
		if [ -n "${FAKE_FAIL}" ]; then
			echo "error: could not build ${1:-all}" >&2
			exit 1
		fi
		if [ "${op}" = "-Syu" ]; then
			touch "${state}/fake-upgraded"
		else
			for name in "$@"; do echo "${name}" >> "${state}/fake-added"; done
		fi
		echo ":: Processing package changes..."
		;;
	*)
		echo "fake yay: unsupported operation ${op}" >&2
		exit 1
		;;
esac
