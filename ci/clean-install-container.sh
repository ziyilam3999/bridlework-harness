#!/bin/sh
# Clean-machine run in a container. Usage: ci/clean-install-container.sh <dist-dir> <source-archive>
# A third argument "twice" runs twice.sh all (install twice beside a seeded overlay) instead.
# Runs e2e.sh all with no network, an empty tmpfs HOME, a non-root uid and exactly three read-only mounts:
# the dist folder, the source archive and ci/clean-install/. It never mounts the repo root or its target/.
# The image is built from ci/clean-install/Dockerfile (a digest-pinned rust image plus clippy; network at build time only) and holds
# bash, git, python3 and the 1.99.1 toolchain with CARGO_HOME/RUSTUP_HOME outside /h. The test run itself has no network.
# Exit 0 ok, 1 test failed, 2 setup error (docker missing or daemon down).
IMAGE="bridlework-clean-install:3677"
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) || exit 2
dist="$1"
srcarc="$2"
script=e2e.sh
if [ "$#" -eq 3 ] && [ "$3" = "twice" ]; then
  script=twice.sh
elif [ "$#" -ne 2 ]; then
  echo "clean-install-container: usage: ci/clean-install-container.sh <dist-dir> <source-archive> [twice]" >&2
  exit 2
fi
case "$dist" in /*) ;; *) echo "clean-install-container: dist-dir must be absolute" >&2; exit 2 ;; esac
case "$srcarc" in /*) ;; *) echo "clean-install-container: source-archive must be absolute" >&2; exit 2 ;; esac
command -v docker >/dev/null 2>&1 || { echo "clean-install-container: docker missing" >&2; exit 2; }
docker info >/dev/null 2>&1 || { echo "clean-install-container: docker daemon down" >&2; exit 2; }
docker build -q -t "$IMAGE" -f "$here/clean-install/Dockerfile" "$here/clean-install" >/dev/null 2>&1 || { echo "clean-install-container: image build failed" >&2; exit 2; }
echo "mount_source=$dist"
echo "mount_source=$srcarc"
echo "mount_source=$here/clean-install"
exec docker run --rm --network none --user 1000:1000 --tmpfs /h:exec,uid=1000,gid=1000 -e HOME=/h \
  -v "$dist":/dist:ro -v "$srcarc":/src.tar.gz:ro -v "$here/clean-install":/ci:ro \
  "$IMAGE" sh "/ci/$script" all /dist /src.tar.gz
