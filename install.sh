#!/bin/sh
#
# install.sh — fetch a released `msm` binary and put it somewhere you can run it.
#
#   sh install.sh              # install the newest release
#   sh install.sh v0.1.0       # install one particular release
#
# What it does, in order: work out which architecture this machine is, ask
# GitHub for the release, download the tarball and the SHA256SUMS file that goes
# with it, check the tarball's checksum against that file, and only then unpack
# the binary into the install directory.
#
# It never needs root and never calls sudo. The default install directory is
# ~/.local/bin, which is inside your home directory; set MSM_INSTALL_DIR to put
# it somewhere else.
#
# Written for POSIX sh — dash, busybox ash, ksh and bash in sh mode all run it,
# so it does not rely on anything bash adds on top of the standard.

# -e: stop at the first command that fails, so a failed download can never be
#     mistaken for a successful install.
# -u: treat an unset variable as an error, which catches typos in variable names.
set -eu

# ---------------------------------------------------------------------------
# Settings
# ---------------------------------------------------------------------------

# Where the releases live. Overridable mostly so that a fork can reuse this
# script without editing it.
REPO="${MSM_REPO:-worxbend/multistream-manager}"

# The name of the program, which is also the name of the file inside the
# tarball and the name it is installed under.
BINARY="msm"

# Where the binary ends up. ~/.local/bin is the usual place for a program a
# single user installed for themselves, and most distributions already put it
# on PATH.
INSTALL_DIR="${MSM_INSTALL_DIR:-$HOME/.local/bin}"

# The two architectures we publish binaries for. Kept here so the error message
# and the actual choice can never drift apart.
SUPPORTED_ARCHES="x86_64 (Intel and AMD desktops) and aarch64 / arm64 (64-bit ARM)"

# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------

# Ordinary progress output. Everything the script is about to do is announced
# through this before it happens, so an interrupted run is easy to make sense of.
say() {
	printf '%s\n' "$*"
}

# Something worth noticing that is not fatal.
warn() {
	printf 'warning: %s\n' "$*" >&2
}

# Something fatal. Prints to standard error and stops the script, which fires
# the cleanup trap below.
die() {
	printf 'error: %s\n' "$*" >&2
	exit 1
}

# Is this command available?
have() {
	command -v "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------------------
# Temporary directory, removed however the script ends
# ---------------------------------------------------------------------------

# Set before the trap is installed so that the cleanup function can always read
# it, even if the script dies before the directory has been created.
tmpdir=""
partial_install=""

cleanup() {
	if [ -n "$tmpdir" ] && [ -d "$tmpdir" ]; then
		rm -rf "$tmpdir"
	fi
	# A half-copied binary under the install directory would be confusing to
	# find later, so remove that too if the copy was interrupted.
	if [ -n "$partial_install" ] && [ -f "$partial_install" ]; then
		rm -f "$partial_install"
	fi
	return 0
}

# EXIT covers both a normal finish and any failure under `set -e`. The three
# signals are trapped separately because a signal handler that returns without
# exiting would let the script carry on from where it was interrupted; calling
# exit instead stops properly. The codes are the usual 128 + signal number.
trap cleanup EXIT
trap 'cleanup; exit 129' HUP
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# ---------------------------------------------------------------------------
# Everything that *does* something lives inside main(), which is only called
# on the very last line of the file. This matters because the documented way
# to run this script is `curl ... | sh`: the shell executes whatever it has
# received so far, so if the connection drops mid-transfer, a plain
# top-to-bottom script would run an arbitrary prefix of itself — possibly
# ending on a truncated command. With the main() wrapper, a partial download
# only ever *defines* an incomplete function (a syntax error at worst) and the
# final `main "$@"` line, the one that starts the work, is by definition the
# last thing to arrive. Nothing above this point runs anything either: it only
# sets variables, defines helpers and installs the cleanup trap.
# ---------------------------------------------------------------------------
main() {

# ---------------------------------------------------------------------------
# Arguments
# ---------------------------------------------------------------------------

usage() {
	cat <<EOF
Usage: sh install.sh [VERSION]

Installs the msm binary from a GitHub release.

  VERSION   A release tag such as v0.1.0, or the bare version 0.1.0.
            Left out, the newest release is used.

Environment:
  MSM_INSTALL_DIR   Where to install (default: \$HOME/.local/bin)
  MSM_REPO          owner/name of the GitHub repository to install from
EOF
}

requested_version="${1:-}"

case "$requested_version" in
	-h | --help)
		usage
		exit 0
		;;
	-*)
		usage >&2
		die "unknown option '$requested_version'"
		;;
esac

# ---------------------------------------------------------------------------
# Step 1: is this a machine we publish binaries for?
# ---------------------------------------------------------------------------

kernel="$(uname -s)"
if [ "$kernel" != "Linux" ]; then
	die "msm is only released for Linux, but this machine reports '$kernel'.
       Build it from source instead: cargo install --git https://github.com/$REPO"
fi

machine="$(uname -m)"
case "$machine" in
	x86_64)
		target="x86_64-unknown-linux-gnu"
		;;
	aarch64 | arm64)
		target="aarch64-unknown-linux-gnu"
		;;
	*)
		die "unsupported architecture '$machine'.
       msm is released for $SUPPORTED_ARCHES only.
       Build it from source instead: cargo install --git https://github.com/$REPO"
		;;
esac

say "Architecture $machine detected, so the target is $target."

# ---------------------------------------------------------------------------
# Step 2: which tools are available for downloading and unpacking?
# ---------------------------------------------------------------------------

if have curl; then
	downloader="curl"
elif have wget; then
	downloader="wget"
else
	die "neither curl nor wget is installed, and one of them is needed to download the release.
       Install either one and run this script again."
fi

have tar || die "tar is not installed, and it is needed to unpack the release."

# Both fetch helpers below apply the same three protections, because a download
# is the only point where this script trusts the network:
#
#   * HTTPS only, including across redirects, so a redirect cannot downgrade the
#     transfer to plain HTTP.
#   * A connect and a stall timeout, so a wedged connection fails with a message
#     instead of hanging with no output at all.
#   * A couple of retries, since a transient failure here is common and cheap to
#     recover from.
#
# wget's spellings differ from curl's, and BusyBox wget (Alpine, many container
# images) understands neither the long options nor --https-only, so the wget
# branch is written with short options and its extras are probed rather than
# assumed.
WGET_OPTS="-q"
if wget --help 2>&1 | grep -q -- "--https-only"; then
	WGET_OPTS="$WGET_OPTS --https-only --timeout=30 --tries=3"
fi

# shellcheck disable=SC2086  # WGET_OPTS is a deliberate word list, not a path.
fetch_stdout() {
	case "$downloader" in
		curl) curl --fail --silent --show-error --location \
			--proto '=https' --proto-redir '=https' --tlsv1.2 \
			--connect-timeout 15 --max-time 300 --retry 2 "$1" ;;
		wget) wget $WGET_OPTS -O - "$1" ;;
	esac
}

# Ask for a URL and report only the HTTP status code, so a "no releases yet"
# 404 can be told apart from a genuine network failure. Prints 000 when the
# request never got a response at all.
fetch_status() {
	case "$downloader" in
		curl) curl --silent --show-error --location --output /dev/null \
			--proto '=https' --proto-redir '=https' --tlsv1.2 \
			--connect-timeout 15 --max-time 60 --write-out '%{http_code}' "$1" 2>/dev/null ||
			echo "000" ;;
		wget)
			# wget prints the status line to stderr; pull the code out of it.
			# BusyBox wget words this differently, so fall back to 000 rather
			# than guessing.
			out="$(wget --server-response --spider "$1" 2>&1 || true)"
			printf '%s\n' "$out" |
				sed -n 's|^[[:space:]]*HTTP/[0-9.]*[[:space:]]\([0-9][0-9][0-9]\).*|\1|p' |
				tail -n 1 |
				grep . || echo "000"
			;;
	esac
}

# Fetch a URL into a file.
# shellcheck disable=SC2086  # WGET_OPTS is a deliberate word list, not a path.
fetch_file() {
	case "$downloader" in
		curl) curl --fail --silent --show-error --location \
			--proto '=https' --proto-redir '=https' --tlsv1.2 \
			--connect-timeout 15 --max-time 600 --retry 2 --output "$2" "$1" ;;
		wget) wget $WGET_OPTS -O "$2" "$1" ;;
	esac
}

say "Using $downloader to download."

# ---------------------------------------------------------------------------
# Step 3: work out which release to install
# ---------------------------------------------------------------------------

if [ -n "$requested_version" ]; then
	tag="$requested_version"
	say "Installing the version you asked for: $tag."
else
	api_url="https://api.github.com/repos/$REPO/releases/latest"
	say "Asking GitHub which release is newest: $api_url"

	# Tell "there are no releases" apart from "the network is broken".
	#
	# The API answers 404 for a repository that has no published release, which
	# is exactly what the very first user to run this sees. Reporting that as a
	# network problem sends them to debug their connection over something that
	# is working perfectly.
	http_status="$(fetch_status "$api_url")"
	case "$http_status" in
		200) ;;
		404)
			die "$REPO has no published release yet, so there is nothing to install.
       Build it from source in the meantime:
         cargo install --git https://github.com/$REPO
       Or, once a release exists, name a version: sh install.sh v0.1.0"
			;;
		403|429)
			die "GitHub rate-limited this request (HTTP $http_status).
       Wait a few minutes, or pass a version explicitly to skip the lookup:
         sh install.sh v0.1.0"
			;;
		000|"")
			die "could not reach the GitHub API to find the newest release.
       Check your network, or pass a version explicitly: sh install.sh v0.1.0"
			;;
		*)
			die "the GitHub API answered unexpectedly (HTTP $http_status).
       Pass a version explicitly to skip the lookup: sh install.sh v0.1.0"
			;;
	esac

	if ! release_json="$(fetch_stdout "$api_url")"; then
		die "could not read the release information from GitHub.
       Pass a version explicitly instead: sh install.sh v0.1.0"
	fi
	# The API answers with JSON. Rather than depend on a JSON parser being
	# installed, pull out the one field we need with sed: the first
	# "tag_name": "..." in the document.
	tag="$(printf '%s\n' "$release_json" |
		sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' |
		head -n 1)"
	[ -n "$tag" ] || die "GitHub did not report a release tag for $REPO.
       There may be no published release yet. You can pass a version explicitly: sh install.sh v0.1.0"
fi

# Accept either "v0.1.0" or "0.1.0". The tag carries the "v", the version in
# the file names does not.
case "$tag" in
	v*)
		version="${tag#v}"
		;;
	*)
		version="$tag"
		tag="v$tag"
		;;
esac

tarball="$BINARY-$version-$target.tar.gz"
base_url="https://github.com/$REPO/releases/download/$tag"

say "Release $tag it is, which means the file $tarball."

# ---------------------------------------------------------------------------
# Step 4: download the tarball and its checksums
# ---------------------------------------------------------------------------

tmpdir="$(mktemp -d 2>/dev/null || mktemp -d -t msm-install)"

say "Downloading $base_url/$tarball"
fetch_file "$base_url/$tarball" "$tmpdir/$tarball" ||
	die "could not download $tarball from release $tag.
       Check that the release exists and that it has a build for $target:
       https://github.com/$REPO/releases"

say "Downloading $base_url/SHA256SUMS"
fetch_file "$base_url/SHA256SUMS" "$tmpdir/SHA256SUMS" ||
	die "could not download SHA256SUMS from release $tag.
       Without it the download cannot be verified, so nothing has been installed."

# ---------------------------------------------------------------------------
# Step 5: verify the download before trusting it
# ---------------------------------------------------------------------------

# SHA256SUMS lists every file in the release, one per line, as a hash followed
# by the file name. Pick out the line for the file we actually downloaded.
# The leading "*" that some tools write in front of the name means "this hash
# was taken in binary mode"; it is not part of the name, so strip it.
expected="$(awk -v name="$tarball" '
	{ file = $2; sub(/^\*/, "", file); if (file == name) print $1 }
' "$tmpdir/SHA256SUMS" | head -n 1)"

[ -n "$expected" ] || die "SHA256SUMS in release $tag has no entry for $tarball, so the download cannot be verified.
       Nothing has been installed."

say "Verifying the checksum of $tarball."

if have sha256sum; then
	actual="$(sha256sum "$tmpdir/$tarball" | cut -d ' ' -f 1)"
elif have shasum; then
	actual="$(shasum -a 256 "$tmpdir/$tarball" | cut -d ' ' -f 1)"
else
	die "neither sha256sum nor shasum is installed, so the download cannot be verified.
       Install one of them and run this script again. Nothing has been installed."
fi

if [ "$actual" != "$expected" ]; then
	die "checksum mismatch for $tarball.
       expected: $expected
       actual:   $actual
       The download is corrupt or has been tampered with. Nothing has been installed."
fi

say "Checksum matches."

# ---------------------------------------------------------------------------
# Step 6: unpack
# ---------------------------------------------------------------------------

say "Unpacking $tarball."
mkdir -p "$tmpdir/unpacked"
tar -xzf "$tmpdir/$tarball" -C "$tmpdir/unpacked" ||
	die "could not unpack $tarball."

# The tarball is flat: the binary sits at the top level next to README.md and
# LICENSE.
unpacked_binary="$tmpdir/unpacked/$BINARY"
[ -f "$unpacked_binary" ] || die "$tarball does not contain a file called $BINARY, which should not happen.
       Please report this at https://github.com/$REPO/issues"

# ---------------------------------------------------------------------------
# Step 7: install
# ---------------------------------------------------------------------------

if [ ! -d "$INSTALL_DIR" ]; then
	say "Creating $INSTALL_DIR, which does not exist yet."
	mkdir -p "$INSTALL_DIR" ||
		die "could not create $INSTALL_DIR.
       Pick somewhere you can write to instead, for example:
       MSM_INSTALL_DIR=\"\$HOME/bin\" sh install.sh"
fi

say "Installing to $INSTALL_DIR/$BINARY"

[ -w "$INSTALL_DIR" ] || die "$INSTALL_DIR is not writable by you, and this script will not use sudo.
       Pick somewhere you can write to instead, for example:
       MSM_INSTALL_DIR=\"\$HOME/bin\" sh install.sh"

# Copy to a temporary name in the same directory and then rename it into place.
# Renaming within one directory happens in a single step, so an interrupted
# install cannot leave a half-written binary where a working one used to be —
# and it also avoids the "text file busy" error you get from writing over a
# copy of msm that is running right now.
partial_install="$INSTALL_DIR/.$BINARY.install.$$"
cp "$unpacked_binary" "$partial_install" ||
	die "could not copy the binary into $INSTALL_DIR."
chmod 0755 "$partial_install"
mv -f "$partial_install" "$INSTALL_DIR/$BINARY" ||
	die "could not move the binary into place in $INSTALL_DIR."
partial_install=""

say "Installed $BINARY $version to $INSTALL_DIR/$BINARY"

# ---------------------------------------------------------------------------
# Step 8: is it actually reachable?
# ---------------------------------------------------------------------------

case ":${PATH:-}:" in
	*":$INSTALL_DIR:"*)
		say "$INSTALL_DIR is already on your PATH, so you can run: $BINARY"
		;;
	*)
		warn "$INSTALL_DIR is not on your PATH, so typing '$BINARY' will not find it yet."
		# Guess the right startup file from the login shell, and print the
		# exact line to add. Fish uses a different syntax from the
		# Bourne-style shells, so it gets its own answer.
		shell_name="$(basename "${SHELL:-sh}")"
		# The single quotes below are deliberate: the "$PATH" in the line we
		# print must reach the user's screen literally, because it belongs in
		# their startup file and is expanded by their shell, not by this one.
		# shellcheck disable=SC2016
		case "$shell_name" in
			fish)
				printf '\nAdd this to %s:\n\n' "$HOME/.config/fish/config.fish"
				printf '  fish_add_path %s\n\n' "$INSTALL_DIR"
				;;
			zsh)
				printf '\nAdd this line to %s, then open a new terminal:\n\n' "$HOME/.zshrc"
				printf '  export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
				;;
			bash)
				printf '\nAdd this line to %s, then open a new terminal:\n\n' "$HOME/.bashrc"
				printf '  export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
				;;
			*)
				printf "\nAdd this line to your shell's startup file, then open a new terminal:\n\n"
				printf '  export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
				;;
		esac
		say "Until then you can run it by its full path: $INSTALL_DIR/$BINARY"
		;;
esac

say ""
say "Next: run '$BINARY login all' to authorise Twitch and YouTube, then '$BINARY' to open the interface."

}

# The only line in the file that starts any work — see the note above main().
main "$@"
