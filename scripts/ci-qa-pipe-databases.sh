#!/usr/bin/env bash
# Disposable PostgreSQL 17 + MySQL 8.4, TLS certificates and real API QA.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
api="$repo/target/debug/firecrab-api"
output="$repo/target/qa/pipe-databases.json"
browser=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --api) api=$2; shift 2 ;;
    --output) output=$2; shift 2 ;;
    --browser) browser=(--browser --frontend-dist "$repo/firecrab-frontend/dist"); shift ;;
    *) printf 'Usage: %s [--api BINARY] [--output REPORT.json] [--browser]\n' "$0" >&2; exit 2 ;;
  esac
done
[[ $(uname -s) == Linux && -x "$api" ]] || { echo 'Requires Linux and a built firecrab-api binary' >&2; exit 2; }
for command in docker openssl python3 psql mysql; do
  command -v "$command" >/dev/null || { echo "Missing required command: $command" >&2; exit 2; }
done
docker info >/dev/null
temporary=$(mktemp -d)
suffix=$(python3 -c 'import uuid; print(uuid.uuid4().hex[:12])')
postgres="qa-pipe-postgres-$suffix"
mysql="qa-pipe-mysql-$suffix"
cleanup() {
  local result=$?
  trap - EXIT
  docker rm -fv "$postgres" "$mysql" >/dev/null 2>&1 || true
  if ! docker info >/dev/null 2>&1; then
    echo 'WARNING cannot verify test container cleanup: Docker is unavailable' >&2
    [[ $result -ne 0 ]] || result=1
  else
    for container in "$postgres" "$mysql"; do
      if docker inspect "$container" >/dev/null 2>&1; then
        echo "WARNING test container remains: $container" >&2
        [[ $result -ne 0 ]] || result=1
      fi
    done
  fi
  rm -rf "$temporary"
  exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

openssl req -x509 -newkey rsa:2048 -nodes -days 1 -keyout "$temporary/ca.key" -out "$temporary/ca.pem" -subj '/CN=Firecrab disposable QA CA' -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign,cRLSign' >"$temporary/openssl.log" 2>&1
openssl req -newkey rsa:2048 -nodes -keyout "$temporary/server.key" -out "$temporary/server.csr" -subj '/CN=localhost' >>"$temporary/openssl.log" 2>&1
cat >"$temporary/extensions" <<'EOF'
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1
EOF
openssl x509 -req -in "$temporary/server.csr" -CA "$temporary/ca.pem" -CAkey "$temporary/ca.key" -CAcreateserial -days 1 -extfile "$temporary/extensions" -out "$temporary/server.crt" >>"$temporary/openssl.log" 2>&1

# These credentials are valid only for this run's loopback test containers.
password=pipe-qa-only-password
docker run -d --name "$postgres" --tmpfs /var/lib/postgresql/data:rw,nosuid,size=256m -e POSTGRES_USER=firecrab_test -e POSTGRES_PASSWORD="$password" -e POSTGRES_DB=pipe_test -p 127.0.0.1::5432 postgres:17 >/dev/null
docker run -d --name "$mysql" --tmpfs /var/lib/mysql:rw,nosuid,size=1g -e MYSQL_ROOT_PASSWORD="$password" -e MYSQL_USER=firecrab_test -e MYSQL_PASSWORD="$password" -e MYSQL_DATABASE=pipe_test -p 127.0.0.1::3306 mysql:8.4 >/dev/null
wait_database() {
  local container=$1
  shift
  for _ in $(seq 1 90); do
    if docker exec "$container" "$@" >/dev/null 2>&1; then return; fi
    [[ $(docker inspect -f '{{.State.Running}}' "$container") == true ]] || break
    sleep 1
  done
  docker logs "$container" >&2
  echo "Database did not become ready: $container" >&2
  return 1
}
wait_database "$postgres" env PGPASSWORD="$password" psql -h127.0.0.1 -U firecrab_test -d pipe_test -c 'SELECT 1'
wait_database "$mysql" env MYSQL_PWD="$password" mysql --protocol=TCP -h127.0.0.1 -ufirecrab_test -Dpipe_test -e 'SELECT 1'

for container in "$postgres" "$mysql"; do
  docker cp "$temporary/server.crt" "$container:/tmp/qa-server.crt"
  docker cp "$temporary/server.key" "$container:/tmp/qa-server.key"
  docker cp "$temporary/ca.pem" "$container:/tmp/qa-ca.pem"
done
docker exec -u 0 "$postgres" sh -c 'cp /tmp/qa-server.crt /var/lib/postgresql/data/server.crt; cp /tmp/qa-server.key /var/lib/postgresql/data/server.key; chown postgres:postgres /var/lib/postgresql/data/server.*; chmod 600 /var/lib/postgresql/data/server.key'
docker exec "$postgres" psql -U firecrab_test -d pipe_test -v ON_ERROR_STOP=1 -c "ALTER SYSTEM SET ssl = on"
docker exec "$postgres" psql -U firecrab_test -d pipe_test -v ON_ERROR_STOP=1 -c 'SELECT pg_reload_conf()'
docker exec -u 0 "$mysql" sh -c 'cp /tmp/qa-server.crt /var/lib/mysql/server-cert.pem; cp /tmp/qa-server.key /var/lib/mysql/server-key.pem; cp /tmp/qa-ca.pem /var/lib/mysql/ca.pem; chown mysql:mysql /var/lib/mysql/server-cert.pem /var/lib/mysql/server-key.pem /var/lib/mysql/ca.pem; chmod 600 /var/lib/mysql/server-key.pem'
docker exec -e MYSQL_PWD="$password" "$mysql" mysql -uroot -e 'ALTER INSTANCE RELOAD TLS'

postgres_port=$(docker inspect -f '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$postgres")
mysql_port=$(docker inspect -f '{{(index (index .NetworkSettings.Ports "3306/tcp") 0).HostPort}}' "$mysql")
export FIRECRAB_TEST_POSTGRES_URI="postgresql://firecrab_test:$password@127.0.0.1:$postgres_port/pipe_test"
export FIRECRAB_TEST_MYSQL_URI="mysql://firecrab_test:$password@127.0.0.1:$mysql_port/pipe_test"
export FIRECRAB_TEST_POSTGRES_CA="$temporary/ca.pem" FIRECRAB_TEST_MYSQL_CA="$temporary/ca.pem"
python3 "$repo/scripts/test-pipe-databases.py" --api "$api" --output "$output" --require-tls "${browser[@]}"
