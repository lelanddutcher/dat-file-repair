#!/bin/bash
# Runs ON THE NAS. Reads DAT paths on stdin. Copies each DAT plus up to 2 healthy neighbours
# before and after it (by filename order, <8 GB) into $DEST, keeping relative paths. Read-only on source.
SRC=/mnt/storage0/zpool; DEST=/mnt/storage0/cliplogger-testing-disk/dat-file-repair/corpus
while IFS= read -r f; do
  rel=${f#$SRC/}; d=$(dirname "$f"); b=$(basename "$f"); out="$DEST/$(dirname "$rel")"; mkdir -p "$out"
  [ -e "$out/$b" ] || cp -p "$f" "$out/$b"
  { ls "$d" | grep -iE '\.mp4$'; echo "$b"; } | sort -f | awk -v t="$b" '{a[NR]=$0; if($0==t)i=NR} END{for(k=i-6;k<=i+6;k++) if(k!=i && k in a) print (k<i?i-k:k-i)"\t"a[k]}' | sort -n | cut -f2 |
  while IFS= read -r r; do
    s=$(stat -c %s "$d/$r"); [ "$s" -lt 8000000000 ] || continue
    echo "$r"; done | head -4 | while IFS= read -r r; do [ -e "$out/$r" ] || cp -p "$d/$r" "$out/$r"; done
done
echo CORPUS_DONE; du -sh "$DEST"
