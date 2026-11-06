#!/bin/sh
# One line on a Linux server: fetches pstd-bridge for this machine and starts its setup.
#   curl -fsSL https://f1xyegb.github.io/pstd/bridge.sh | sudo sh
set -eu

case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) echo "pstd-bridge: this processor isn't supported yet: $(uname -m)" >&2; exit 1 ;;
esac

if [ "$(uname -s)" != Linux ]; then
  echo "pstd-bridge: this line is for a Linux server" >&2
  exit 1
fi
if [ "$(id -u)" != 0 ]; then
  echo "pstd-bridge: run it with sudo" >&2
  exit 1
fi

url="https://github.com/f1xyegb/pstd-bridge/releases/latest/download/pstd-bridge-linux-$arch"
curl -fsSL "$url" -o /usr/local/bin/pstd-bridge
chmod 755 /usr/local/bin/pstd-bridge

# the script itself arrives on stdin: the questions need the terminal
exec /usr/local/bin/pstd-bridge setup </dev/tty
