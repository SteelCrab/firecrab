#!/bin/sh
# Prints the two tables firecrab-net-helper owns, if present. Requires nft.
set -eu

echo "== inet firecrab =="
sudo nft list table inet firecrab 2>&1 || echo "(not present)"

echo
echo "== bridge firecrab_l2 =="
sudo nft list table bridge firecrab_l2 2>&1 || echo "(not present)"
