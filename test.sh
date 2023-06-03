set -xeuo pipefail

LEASE_NAME="$USER-$(date +%s)"
TEMP_DIR="$(mktemp -d)"
FIRST_FILE="$TEMP_DIR/first"
SECOND_FILE="$TEMP_DIR/second"

cargo build --release

./target/release/just-one-init \
	--lease-name="$LEASE_NAME" \
	--hostname first \
	--pod-namespace default \
	bash -- -c "test ! -e \"$SECOND_FILE\" && echo hello world > \"$FIRST_FILE\"" &
./target/release/just-one-init \
	--lease-name="$LEASE_NAME" \
	--hostname second \
	--pod-namespace default \
	bash -- -c "test ! -e \"$FIRST_FILE\" && echo hello world > \"$SECOND_FILE\"" &
sleep 1

while [ ! -e "$FIRST_FILE" ] && [ ! -e "$SECOND_FILE" ]; do
	sleep 1
done

sleep 1

if [ -e "$FIRST_FILE" ] && [ -e "$SECOND_FILE" ]; then
	echo "Did not lock properly"
	exit 1
else
	echo "Locked properly"
	exit 0
fi
