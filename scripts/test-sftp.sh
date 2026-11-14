#!/bin/sh
# Isolated localhost sshd, generated fixture keys, separate agent; no ~/.ssh edits.
set -eu
cd "$(dirname "$0")/.."
fixture=$(mktemp -d /tmp/rdp-manager-sftp.XXXXXXXX)
sshd_pid=
agent_pid=
cleanup() {
    if [ -n "$sshd_pid" ]; then kill "$sshd_pid" 2>/dev/null || true; wait "$sshd_pid" 2>/dev/null || true; fi
    if [ -n "$agent_pid" ]; then kill "$agent_pid" 2>/dev/null || true; wait "$agent_pid" 2>/dev/null || true; fi
    rm -rf "$fixture"
}
trap cleanup EXIT HUP INT TERM
ssh-keygen -q -t ed25519 -N '' -f "$fixture/host"
ssh-keygen -q -t ed25519 -N '' -f "$fixture/client"
ssh-keygen -q -t ed25519 -N 'synthetic-key-passphrase' -f "$fixture/encrypted"
cat "$fixture/client.pub" "$fixture/encrypted.pub" > "$fixture/authorized_keys"
mkdir "$fixture/store"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
cat > "$fixture/sshd_config" <<EOF
Port $port
ListenAddress 127.0.0.1
HostKey $fixture/host
PidFile $fixture/sshd.pid
AuthorizedKeysFile $fixture/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
UsePAM no
StrictModes no
Subsystem sftp internal-sftp
EOF
/usr/sbin/sshd -t -f "$fixture/sshd_config"
/usr/sbin/sshd -D -f "$fixture/sshd_config" -E "$fixture/sshd.log" &
sshd_pid=$!
export SSH_AUTH_SOCK="$fixture/agent.sock"
ssh-agent -D -a "$SSH_AUTH_SOCK" > "$fixture/agent.log" 2>&1 &
agent_pid=$!
for attempt in $(seq 1 50); do
    if [ -S "$SSH_AUTH_SOCK" ] && [ -f "$fixture/sshd.pid" ]; then break; fi
    sleep 0.1
done
ssh-add "$fixture/client" < /dev/null
export RDP_MANAGER_SFTP_FIXTURE="$fixture"
export RDP_MANAGER_SFTP_PORT="$port"
export RDP_MANAGER_SFTP_USER="$(id -un)"
if ! cargo test --locked --offline sftp_live -- --ignored; then
    cat "$fixture/sshd.log"
    exit 1
fi
