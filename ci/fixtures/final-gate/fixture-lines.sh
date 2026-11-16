#!/bin/sh
# Planted failing gate program (test data only). Prints 50 numbered lines, then fails.
i=1
while [ "$i" -le 50 ]; do
  printf 'row-%02d\n' "$i"
  i=$((i + 1))
done
exit 1
