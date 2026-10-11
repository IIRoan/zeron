#!/bin/sh
# Shared Muute infrastructure; always use the registered main checkout.
set -eu
: "${ZERON_PROJECT_ROOT:?Set ZERON_PROJECT_ROOT to the Muute main checkout}"
muute_helper_directory=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$ZERON_PROJECT_ROOT"
muute_database_project=$(basename "$ZERON_PROJECT_ROOT")
muute_redis_project="$muute_database_project-redis"
export COMPOSE_PROJECT_NAME="$muute_database_project"

case "${1:-run}" in
  ready)
    docker compose -f compose.db-only.yaml exec -T db pg_isready -U postgres -d postgres
    docker compose -p "$muute_redis_project" -f compose.redis.yaml -f "$muute_helper_directory/muute-redis.compose.yaml" exec -T redis redis-cli ping
    bun -e '
      import { createConnection } from "node:net";
      await Promise.all([5432, 6379, 8025, 1025, 10000, 10001, 10002].map(port =>
        new Promise((resolve, reject) => {
          const socket = createConnection({ host: "127.0.0.1", port });
          socket.setTimeout(1000, () => socket.destroy(new Error(`Port ${port} is not ready`)));
          socket.once("error", reject);
          socket.once("connect", () => { socket.destroy(); resolve(); });
        })
      ));
      console.log("PostgreSQL, Redis, Mailpit and Azurite are ready");
    '
    ;;
  run)
    # One project for PostgreSQL/Mailpit/Azurite and another for shared Redis.
    # Existing containers and named database volumes are preserved.
    stop_muute_infrastructure() {
      trap - EXIT INT TERM
      docker compose -f compose.db-only.yaml stop -t 1 &
      muute_database_stop=$!
      docker compose -p "$muute_redis_project" -f compose.redis.yaml -f "$muute_helper_directory/muute-redis.compose.yaml" stop -t 1 redis &
      muute_redis_stop=$!
      wait "$muute_database_stop" || true
      wait "$muute_redis_stop" || true
    }
    trap stop_muute_infrastructure EXIT
    trap 'exit 0' INT TERM
    docker compose -p "$muute_redis_project" -f compose.redis.yaml -f "$muute_helper_directory/muute-redis.compose.yaml" up -d --no-recreate redis
    bun run start-db --detach --no-recreate
    # Keep a foreground service and its output without Compose's attached-up
    # interrupt handler removing pre-existing containers on Podman.
    docker compose -f compose.db-only.yaml logs --follow --tail 100
    echo "Muute infrastructure output ended unexpectedly" >&2
    exit 1
    ;;
  *)
    echo "Usage: muute-database-service.sh [run|ready]" >&2
    exit 2
    ;;
esac
