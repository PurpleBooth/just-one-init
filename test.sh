#!/usr/bin/env bash

set -euo pipefail

LEASE_NAME="$USER-$(date +%s)"
TEMP_DIR="$(mktemp -d)"
FIRST_FILE="$TEMP_DIR/first"
SECOND_FILE="$TEMP_DIR/second"

cargo build --release

./target/release/just-one-init \
	--lease-name="$LEASE_NAME" \
	--listen-addr="127.0.0.1:5048" \
	--hostname first \
	--pod-namespace default \
	bash -- -c "test ! -e \"$SECOND_FILE\" && echo hello world > \"$FIRST_FILE\"" &

FIRST_PID="$!"
./target/release/just-one-init \
	--listen-addr="127.0.0.1:5047" \
	--lease-name="$LEASE_NAME" \
	--hostname second \
	--pod-namespace default \
	bash -- -c "test ! -e \"$FIRST_FILE\" && echo hello world > \"$SECOND_FILE\"" &
SECOND_PID="$!"

while [ ! -e "$FIRST_FILE" ] && [ ! -e "$SECOND_FILE" ]; do
	sleep 1
done

sleep 1

if ! curl --fail --request GET -sL \
	--url 'http://localhost:5047' && ! curl --fail --request GET -sL \
	--url 'http://localhost:5048'; then
	echo "Server did not start"
	kill "$FIRST_PID" "$SECOND_PID" || true
	exit 1
fi

if [ -e "$FIRST_FILE" ] && [ -e "$SECOND_FILE" ]; then
	echo "Did not lock properly"
	kill "$FIRST_PID" "$SECOND_PID" || true
	exit 1
else
	echo "Locked properly"
	kill "$FIRST_PID" "$SECOND_PID" || true
	exit 0
fi
