#!/bin/bash
# Builds a raw "card image" with one .DAT split in two pieces and random junk between them.
# Usage: make_fragmented_image.sh <dat> <split-bytes> <gap-bytes> <out.img>
# Layout: 4 MiB junk | DAT[0, split) | gap junk | DAT[split, end) | 3 MiB junk.
# Both <split-bytes> and <gap-bytes> should be multiples of 4096 (a cluster).
set -euo pipefail
dat=$1; split=$2; gap=$3; out=$4
size=$(stat -f %z "$dat" 2>/dev/null || stat -c %s "$dat")
rest=$((size - split))
if [ -e "$out" ]; then echo "$out exists" >&2; exit 1; fi
{
  head -c 4194304 /dev/urandom
  head -c "$split" "$dat"
  head -c "$gap" /dev/urandom
  tail -c "$rest" "$dat"
  head -c 3145728 /dev/urandom
} > "$out"
echo "second piece at $((4194304 + split + gap))"
