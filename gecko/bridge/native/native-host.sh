#!/bin/bash

LOG_DIR="/tmp/geckoOS"
LOG_FILE="$LOG_DIR/native-host.log"

mkdir -p "$LOG_DIR"

# Log that Firefox started us
echo "========== $(date -Iseconds) ==========" >> "$LOG_FILE"
echo "GeckoOS native host started via Firefox (PID $$)" >> "$LOG_FILE"

# Exec real binary; keep stdin/stdout for Firefox; stderr → log
exec /home/andrew/geckos/geckos/target/release/geckoOS-native-host 2>>"$LOG_FILE"
