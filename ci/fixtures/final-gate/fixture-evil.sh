#!/bin/sh
# Planted failing gate program (test data only). Prints a workflow-command-looking line and one very long line, then fails.
echo "planted evil output"
echo "::error::planted"
i=0
while [ "$i" -lt 2000 ]; do
  printf x
  i=$((i + 1))
done
printf '\n'
exit 1
