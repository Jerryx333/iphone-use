#!/bin/sh
set -eu
exec python3 "$(dirname "$0")/build-ios.py" "${1:-unsigned}"
