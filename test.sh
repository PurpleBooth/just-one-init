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
	--pod-namespace default -- \
	bash -c "echo hello world > \"$FIRST_FILE\"" &
FIRST_PID="$!"

while [ "$(curl --write-out "%{http_code}\n" --silent --output /dev/null "http://127.0.0.1:5047")" -eq 200 ]; do
	echo "waiting for first to lock"
	sleep 1
done

./target/release/just-one-init \
	--lease-name="$LEASE_NAME" \
	--listen-addr="127.0.0.1:5047" \
	--reelect-after=30min \
	--hostname second \
	--pod-namespace default -- \
	bash -c "echo hello world > \"$SECOND_FILE\"" &
SECOND_PID="$!"

while [ "$(curl --write-out "%{http_code}\n" --silent --output /dev/null "http://127.0.0.1:5048")" -eq 404 ]; do
	echo "waiting for second to follow"
	sleep 1
done

while [ ! -e "$FIRST_FILE" ] && [ ! -e "$SECOND_FILE" ]; do
	sleep 1
done

if [ -e "$FIRST_FILE" ] && [ -e "$SECOND_FILE" ]; then
	echo "Did not lock properly"
	kill "$FIRST_PID" "$SECOND_PID" || true
	exit 1
else
	echo "Locked properly"
	kill "$FIRST_PID" "$SECOND_PID" || true
	exit 0
fi
