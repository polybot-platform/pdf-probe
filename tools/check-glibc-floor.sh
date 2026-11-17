#!/usr/bin/env bash
# Fails if a shared library needs a newer glibc than the one named: `tools/check-glibc-floor.sh lib.so 2.28`.
# The glibc a library needs is the newest GLIBC_x.y version among the symbols it imports, and it comes from the
# machine it was built on, so a library built on a new distribution refuses to load on an older one.
set -euo pipefail

lib="${1:?usage: tools/check-glibc-floor.sh <library> <max glibc version, e.g. 2.28>}"
max="${2:?usage: tools/check-glibc-floor.sh <library> <max glibc version, e.g. 2.28>}"

needed=$(objdump -T "$lib" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -uV | tail -n 1)
[ -n "$needed" ] || { echo "check-glibc-floor: no GLIBC symbol versions in $lib (not a glibc library?)" >&2; exit 1; }
echo "check-glibc-floor: $lib needs glibc $needed (allowed: up to $max)"
newest=$(printf '%s\n%s\n' "$needed" "$max" | sort -V | tail -n 1)
[ "$newest" = "$max" ] || { echo "check-glibc-floor: $needed is newer than $max" >&2; exit 1; }
