#!/bin/sh
# Smoke test of an image: it starts with a read-only root filesystem, as UID 65532, with every
# capability dropped, passes its own health check, and has no shell.
#
#   docker/test/smoke.sh <image>
set -eu

image=${1:?usage: docker/test/smoke.sh <image>}
name="antirion-smoke-$$"
volume="antirion-smoke-$$"

cleanup() {
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker run -d --name "$name" --read-only --user 65532:65532 --cap-drop ALL \
  --health-interval 1s --health-start-period 1s \
  --mount "type=volume,src=$volume,dst=/var/lib/antirion" "$image" >/dev/null

status=starting
for _ in $(seq 1 30); do
  status=$(docker inspect -f '{{.State.Health.Status}}' "$name")
  [ "$status" = healthy ] && break
  sleep 1
done
if [ "$status" != healthy ]; then
  echo "the container did not become healthy ($status)" >&2
  docker logs "$name" >&2
  exit 1
fi

user=$(docker inspect -f '{{.Config.User}}' "$image")
[ "$user" = "65532:65532" ] || { echo "the image runs as '$user', not 65532:65532" >&2; exit 1; }

if docker exec "$name" /bin/sh -c true >/dev/null 2>&1; then
  echo "the image has a shell" >&2
  exit 1
fi

docker exec "$name" /antirion version
echo "smoke test passed: $image"
