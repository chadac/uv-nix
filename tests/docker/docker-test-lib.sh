#!/bin/sh
# Entrypoint for library test containers.
# Usage: docker-test-lib.sh <package> <python-check> [--no-binary]
#
# Creates a project, adds the package, runs the check script.
set -e

PACKAGE="$1"
CHECK="$2"
NO_BINARY="${3:-}"

cd /tmp
uv init --no-progress test-project
cd test-project

if [ "$NO_BINARY" = "--no-binary" ]; then
    uv add --no-progress --no-binary "$PACKAGE" "$PACKAGE"
else
    uv add --no-progress "$PACKAGE"
fi

uv run --no-progress python -c "$CHECK"
