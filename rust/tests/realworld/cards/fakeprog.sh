#!/bin/sh
# A stand-in for a program whose real run would change something (gh, alembic). Linked into
# /usr/local/bin under each name; it records that it ran, with its arguments, in a file the card kit
# reads, and prints one line. A card's merge or migration "ran" exactly when this file has a line.
n=$(basename "$0")
mkdir -p /var/tmp/witness
printf '%s\n' "$*" >> "/var/tmp/witness/$n.log"
echo "$n: ran $*"
