# Firejail profile for Vernam, the Signal client. Wayland only.
#   firejail --profile=scripts/vernam.profile target/linux/release/vernam
# A profile given with --profile replaces firejail's default one, so
# everything this file does not restrict stays reachable: /usr and /etc
# for reading and the network. The paths assume the standard XDG layout;
# with XDG_DATA_HOME pointed somewhere else the mkdir and whitelist
# lines below have to follow it. The executable also has to be visible
# inside the jail: when the checkout lives under home, run a copy from
# outside it (say /usr/local/bin) or whitelist the checkout directory.
#
# `scripts/install-firejail.sh` installs this profile as
# ~/.config/firejail/vernam.profile, the built binary as
# ~/.local/bin/vernam.bin (whitelisted in the installed copy: the jail
# hides the checkout under home), and a ~/.local/bin/vernam wrapper
# that launches it with this profile, so the terminal, the app menu
# and sgnl:// links all run jailed. It also writes the desktop entry
# and icons the app's own registration can't (its writes vanish with
# the jail), with the wrapper in the Exec line.

# Sandbox shape: capabilities dropped, no new privileges or setuid, no
# /dev/input (Wayland input goes through the compositor anyway), no X11
# (the path socket is blacklisted below; see there for the abstract
# one), only unix, internet and netlink sockets (netlink for interface
# enumeration, which call setup needs), seccomp, no new user namespaces,
# private IPC, a private /dev, which keeps dri, snd and video capture
# for calls but on proprietary NVIDIA hosts leaves out the nvidia nodes
# (there, drop private-dev to render), and a private /tmp. dvd, tv and
# FIDO devices have no business here.
caps.drop all
ipc-namespace
noinput
nonewprivs
noroot
nodvd
notv
nou2f
protocol unix,inet,inet6,netlink
seccomp
restrict-namespaces
private-tmp
private-dev
# X11: the path socket under /tmp/.X11-unix is blacklisted (this wins
# over the whitelist private-tmp adds for the display server). The
# abstract socket compositors' Xwayland listens on by default can only
# be hidden by a network namespace, and x11 none refuses the launch
# without one: net none would cut the internet, and a namespace with
# connectivity needs host-side bridge setup. So the abstract socket
# stays reachable and the app has to simply not use it, which GPUI
# does not: it picks Wayland whenever WAYLAND_DISPLAY is set and opens
# no X11 connection then (gpui-pre-linux display_connection.rs). With
# the network expendable, add net none and x11 none for the full block.
blacklist /tmp/.X11-unix
# No netfilter: its default rules fence the UDP ports calls use, and
# calls are the one thing this client does besides text.

# The buses, filtered: on the session bus notifications (with Plasma's
# reply field) and the portals the file dialogs go through; on the
# system bus logind's wake signal, which reconnects the session after
# the machine wakes. Everything else stays out. Both filters need the
# xdg-dbus-proxy binary; without it firejail warns and leaves the bus
# unfiltered.
dbus-system filter
dbus-system.talk org.freedesktop.login1
dbus-user filter
dbus-user.talk org.freedesktop.Notifications
dbus-user.talk org.freedesktop.portal.Desktop

# The user runtime directory: only the compositor, bus and audio
# sockets, as firejail's own whitelist-runuser-common.inc whitelists
# them, so the ssh agent and every other session service in there stays
# invisible. The globs cover the compositor's socket numbering.
whitelist ${RUNUSER}/bus
whitelist ${RUNUSER}/pulse/native
whitelist ${RUNUSER}/pipewire-?
whitelist ${RUNUSER}/wayland-?

# The home directory inside the jail reads as empty except for the lines
# below: what Vernam owns (data, the hand-off socket for sgnl:// links,
# the logs under it), the font and shader caches the graphics stack
# keeps there, and Downloads for the file dialogs (attach and save-as).
# The data directory is ~/.local/share/vernam: the directories crate's
# Linux ProjectDirs ignores the io.github.knpwrs qualifier and
# lowercases the app name (lin.rs, project_dirs_from). Nothing of the
# desktop registration is whitelisted: autostart entries
# and Exec= lines are host-execution mechanisms that outlive the
# sandbox, so the host copies are written by unjailed launches only
# (register_desktop runs on every launch; from inside the jail its
# writes vanish with it, and the open-at-login toggle does nothing).
# A whitelist entry for a path the host does not have yet is dropped
# silently, and the app would then create that path inside the sandbox
# where it dies with the session, so mkdir makes each one real first
# (existing ones stay).
mkdir ${HOME}/.local
mkdir ${HOME}/.local/share
mkdir ${HOME}/.cache
mkdir ${HOME}/.local/share/vernam
mkdir ${HOME}/.cache/fontconfig
mkdir ${HOME}/.cache/mesa_shader_cache
mkdir ${HOME}/.cache/mesa_shader_cache_db
mkdir ${HOME}/Downloads
whitelist ${HOME}/.local/share/vernam
whitelist ${HOME}/.cache/fontconfig
whitelist ${HOME}/.cache/mesa_shader_cache
whitelist ${HOME}/.cache/mesa_shader_cache_db
whitelist ${HOME}/Downloads
