#!/bin/sh
# Test helper for the CLI executor (T13). The first argument selects a behaviour.
case "$1" in
  json)  echo '{"hello":"world","secret":"x","meta":{"ok":true}}' ;;
  echo)  echo "$2" ;;            # echo the second argument
  stdin) cat ;;                  # echo standard input verbatim
  env)   echo "$MY_VAR" ;;       # echo an environment variable
  lines) printf 'a\nb\nc\n' ;;   # three lines
  cwd)   pwd -P ;;               # physical working directory
  fail)  echo "boom" >&2; exit 3 ;;  # non-zero exit with stderr
  signal) kill -9 $$ ;;              # terminate self via SIGKILL (no exit code)
  *)     echo "default" ;;
esac
