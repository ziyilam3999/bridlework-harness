#!/bin/sh
# Clean gate program (test data only). Passes, unless a privacy env name leaked in (then it fails).
if [ -n "${HARNESS_PRIVACY_LIST+x}" ] || [ -n "${HARNESS_PRIVACY_OLD_LIST+x}" ] || [ -n "${HARNESS_PRIVACY_LIST_SECRET+x}" ]; then
  echo "private input reached the gate"
  exit 1
fi
exit 0
