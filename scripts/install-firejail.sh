#!/bin/sh
# Installs Vernam user-level with its firejail jail, so every way of
# launching it jails: the built binary goes to ~/.local/bin/vernam.bin
# (whitelisted in the installed profile; the jail hides the checkout
# under home), a ~/.local/bin/vernam wrapper launches it through
# firejail with the installed profile (a bare `vernam` from a terminal
# would otherwise run it unjailed: the shell reads neither .desktop
# files nor firejail profiles), the profile goes to
# ~/.config/firejail/vernam.profile, and the desktop entry and icons
# that the app's own registration writes only from an unjailed launch
# (crates/app/src/main.rs, register_desktop; from inside the jail its
# writes vanish with it) are written here with the wrapper in the Exec
# line, so the app menu and sgnl:// links jail too.
#
#   scripts/install-firejail.sh            # installs target/linux/release/vernam
#   scripts/install-firejail.sh <binary>   # any other build
#
# Rerun after a build: the copy under ~/.local/bin is a snapshot.
set -eu

cd "$(dirname "$0")/.."

binary=${1:-target/linux/release/vernam}
if [ ! -f "$binary" ]; then
    echo "no binary at $binary; build it first (./build-linux.sh)" >&2
    exit 1
fi
command -v firejail >/dev/null 2>&1 || echo "warning: firejail is not on PATH" >&2

# The binary where the jail can see it, and the wrapper `vernam` finds
# on PATH.
mkdir -p "$HOME/.local/bin"
cp "$binary" "$HOME/.local/bin/vernam.bin"
config=${XDG_CONFIG_HOME:-$HOME/.config}
cat > "$HOME/.local/bin/vernam" <<EOF
#!/bin/sh
exec firejail --profile="$config/firejail/vernam.profile" "\$HOME/.local/bin/vernam.bin" "\$@"
EOF
chmod +x "$HOME/.local/bin/vernam"

# The profile where firejail finds it, with the installed binary
# whitelisted: the jail hides the checkout under home (see the
# profile's header).
mkdir -p "$config/firejail"
cp scripts/vernam.profile "$config/firejail/vernam.profile"
echo 'whitelist ${HOME}/.local/bin/vernam.bin' >> "$config/firejail/vernam.profile"

# What register_desktop writes on an unjailed launch, with the wrapper
# (which jails) in Exec.
data=${XDG_DATA_HOME:-$HOME/.local/share}
mkdir -p "$data/applications"
{
    echo '[Desktop Entry]'
    echo 'Type=Application'
    echo 'Name=Vernam'
    echo 'Comment=Unofficial Signal client'
    echo "Exec=$HOME/.local/bin/vernam %U"
    echo 'StartupWMClass=io.github.knpwrs.vernam'
    echo 'Icon=io.github.knpwrs.vernam'
    echo 'MimeType=x-scheme-handler/sgnl;'
    echo 'Terminal=false'
    echo 'Categories=Network;InstantMessaging;'
} > "$data/applications/io.github.knpwrs.vernam.desktop"
for size in 16x16 24x24 32x32 48x48 64x64 256x256 512x512; do
    mkdir -p "$data/icons/hicolor/$size/apps"
    cp "crates/app/assets/app-icon/$size.png" \
        "$data/icons/hicolor/$size/apps/io.github.knpwrs.vernam.png"
done

# Best effort, as in register_desktop: without xdg-utils the desktop
# file still registers the handler.
xdg-mime default io.github.knpwrs.vernam.desktop x-scheme-handler/sgnl 2>/dev/null || true
