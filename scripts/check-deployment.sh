#!/usr/bin/env bash
set -euo pipefail

# Parse the supplied deployment without starting containers or reading live data.
root="$(git rev-parse --show-toplevel)"
docker compose -f "$root/docker-compose.yml" config --quiet
bash -n "$root/start-test.sh"
printf '%s\n' 'Deployment configuration and startup syntax are valid.'
