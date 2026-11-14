#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
fixture=$(mktemp -d /tmp/rdp-manager-native.XXXXXXXX)
server_pid=
cleanup(){ if [ -n "$server_pid" ];then kill "$server_pid" 2>/dev/null||true;wait "$server_pid" 2>/dev/null||true;fi;rm -rf "$fixture"; }
trap cleanup EXIT HUP INT TERM
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$fixture/key.pem" -out "$fixture/cert.pem" -days 1 -subj '/CN=fixture.local' -addext 'subjectAltName=DNS:fixture.local,IP:127.0.0.1' > "$fixture/openssl.log" 2>&1
openssl req -x509 -newkey rsa:2048 -nodes -keyout "$fixture/changed-key.pem" -out "$fixture/changed-cert.pem" -days 1 -subj '/CN=fixture.local' -addext 'subjectAltName=DNS:fixture.local,IP:127.0.0.1' > "$fixture/changed-openssl.log" 2>&1
cc -std=gnu11 -Wno-deprecated-declarations native/fixture_server.c -o "$fixture/server" $(pkg-config --cflags --libs freerdp3 winpr3)
port=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
export XDG_CONFIG_HOME="$fixture/config"
export XDG_DATA_HOME="$fixture/data"
mkdir "$XDG_CONFIG_HOME" "$XDG_DATA_HOME"
cat > "$fixture/krb5.conf" <<EOF
[libdefaults]
default_realm = RDP.TEST
dns_lookup_kdc = false
dns_lookup_realm = false
EOF
export KRB5_CONFIG="$fixture/krb5.conf"
export KRB5CCNAME="FILE:$fixture/krb5cc"
export KRB5_KTNAME="FILE:$fixture/keytab"
export RDP_MANAGER_NATIVE_FIXTURE="$fixture"
export RDP_MANAGER_NATIVE_PORT="$port"
"$fixture/server" "$port" "$fixture/cert.pem" "$fixture/key.pem" "$fixture/input.log" "$fixture/ready" "$fixture/changed-cert.pem" "$fixture/changed-key.pem" > "$fixture/server.log" 2>&1 &
server_pid=$!
for attempt in $(seq 1 50);do if [ -f "$fixture/ready" ];then break;fi;sleep 0.1;done
# Exercise the actual EGFX/GDI handlers using a connected context/cache.
cc -std=gnu11 -Wno-deprecated-declarations native/test_graphics.c -o "$fixture/test-graphics" $(pkg-config --cflags --libs freerdp-client3 winpr3)
if ! timeout 30s "$fixture/test-graphics" "$port" > "$fixture/graphics.log" 2>&1;then cat "$fixture/graphics.log" "$fixture/server.log";exit 1;fi
rg "^EGFX:" "$fixture/graphics.log"
kill "$server_pid" 2>/dev/null||true
wait "$server_pid" 2>/dev/null||true
rm -f "$fixture/ready"
"$fixture/server" "$port" "$fixture/cert.pem" "$fixture/key.pem" "$fixture/input.log" "$fixture/ready" "$fixture/changed-cert.pem" "$fixture/changed-key.pem" > "$fixture/server.log" 2>&1 &
server_pid=$!
for attempt in $(seq 1 50);do if [ -f "$fixture/ready" ];then break;fi;sleep 0.1;done
if ! cargo test --locked --offline rdp_live -- --ignored > "$fixture/client.log" 2>&1;then cat "$fixture/client.log" "$fixture/server.log";exit 1;fi
rg "test rdp::|test result:" "$fixture/client.log"
