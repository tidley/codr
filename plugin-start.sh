#!/bin/sh
set -eu
host_services=$(uname -s)
if [ "${CODR_SERVICE_MANAGER:-}" = systemd ]; then host_services=Linux; fi
if [ "${CODR_SERVICE_MANAGER:-}" = launchd ]; then host_services=Darwin; fi
if [ "$host_services" = "Darwin" ]; then
    domain="gui/$(id -u)"
    # launchctl print exits 0 for a missing job, so test its output instead.
    [ -n "$(launchctl print "$domain" 2>/dev/null)" ] || domain="user/$(id -u)"
    label="io.github.tidley.codr.herdr-white-noise-bridge"
    if [ -f "$HOME/Library/LaunchAgents/io.github.tidley.codr.codr.plist" ]; then
        label="io.github.tidley.codr.codr"
    fi
    if [ -n "$(launchctl print "$domain/$label" 2>/dev/null)" ]; then
        launchctl kickstart "$domain/$label"
    else
        # A freshly bootstrapped agent starts through RunAtLoad; kicking it during
        # that handover aborts the spawn.
        launchctl bootstrap "$domain" "$HOME/Library/LaunchAgents/$label.plist"
    fi
else
    XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
    DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}
    export XDG_RUNTIME_DIR DBUS_SESSION_BUS_ADDRESS
    # A dedicated user service supervises the plugin worker across herdr client disconnects.
    if [ -f "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/codr.service" ]; then
        systemctl --user start codr.service
    else
        systemctl --user start herdr-white-noise-bridge.service
    fi
fi
if [ "${1:-}" = --show-menu ]; then
    if ! "${HERDR_BIN_PATH:-herdr}" plugin pane open --plugin local.white-noise --entrypoint forwarding --focus; then
        printf '%s\n' 'White Noise forwarding menu could not open. Use the forwarding action or its configured shortcut.' >&2
    fi
fi
