#!/bin/sh
# Capture a #7 (maximize -> app crash) repro from inside a running Pyxis guest.
#
# Run this on the device BEFORE clicking maximize. It snapshots the current end
# of every relevant log, waits while you reproduce, then writes only the lines
# that appeared during the repro to /tmp/pyxis-maximize-repro.txt.
#
# Why a script rather than "send me the logs": evo.log is tens of thousands of
# DEBUG lines, and the interesting window is about twenty of them. This keeps
# the signal.
#
# Usage on the device:
#     sh /usr/sbin/capture-maximize-repro.sh
#   ... then click maximize on an app window and let it crash ...
#   ... press Enter ...
#     cat /tmp/pyxis-maximize-repro.txt

OUT=/tmp/pyxis-maximize-repro.txt
LOGS="/var/log/evo.log /var/log/echo.log /var/log/vela.log"

echo "Marking current log positions..."
for f in $LOGS; do
    [ -f "$f" ] || continue
    eval "MARK_$(basename "$f" .log)=$(wc -l < "$f")"
done

echo
echo "Now reproduce the crash:"
echo "  1. Launch an app from the dock (or: pyxis-appctl launch files)"
echo "  2. Click the MAXIMIZE button on its title bar"
echo "  3. Click it again to unmaximize, if it survived the first click"
echo
printf "Press Enter once the app has crashed... "
read _dummy

{
    echo "=============== PYXIS #7 MAXIMIZE REPRO ==============="
    echo "date: $(date)"
    echo "vela processes still alive: $(pgrep -c vela 2>/dev/null)"
    echo "evo alive: $(pgrep -c evo 2>/dev/null)"
    echo

    for f in $LOGS; do
        [ -f "$f" ] || continue
        base=$(basename "$f" .log)
        eval "start=\$MARK_${base}"
        [ -n "$start" ] || start=0
        echo "=============== $f (from line $start) ==============="
        # Drop the DEBUG firehose from evo; keep everything from the others,
        # since a client's dying words are usually a single un-levelled line.
        if [ "$base" = "evo" ]; then
            tail -n "+$((start + 1))" "$f" | grep -avE 'DEBUG.*(Received Echo|Sent Echo|frame|render)'
        else
            tail -n "+$((start + 1))" "$f"
        fi
        echo
    done
} > "$OUT" 2>&1

echo
echo "Written to $OUT ($(wc -l < "$OUT") lines)."
echo "Copy it off with:  cat $OUT"
