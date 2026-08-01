#!/bin/sh
set -eu
umask 077

pid=${1:-}
if [ "$#" -ne 1 ] || [ -z "$pid" ]; then
  echo "usage: scripts/verify-network-boundary.sh PID" >&2
  exit 64
fi

case "$pid" in
  *[!0-9]* | "")
    echo "PID must be numeric" >&2
    exit 64
    ;;
esac

if [ "$pid" -le 0 ]; then
  echo "PID must be greater than zero" >&2
  exit 64
fi

observed_pid=$(/bin/ps -p "$pid" -o pid= 2>/dev/null | /usr/bin/tr -d ' ')
if [ "$observed_pid" != "$pid" ]; then
  echo "No running process has the requested PID." >&2
  exit 1
fi
command_before=$(/bin/ps -p "$pid" -o command= 2>/dev/null)
if [ -z "$command_before" ]; then
  echo "Could not establish the requested process identity." >&2
  exit 1
fi

base_output=$(/usr/bin/mktemp /tmp/woof-lsof-base.XXXXXX)
socket_output=$(/usr/bin/mktemp /tmp/woof-lsof-sockets.XXXXXX)
socket_records=$(/usr/bin/mktemp /tmp/woof-lsof-records.XXXXXX)
lsof_errors=$(/usr/bin/mktemp /tmp/woof-lsof-errors.XXXXXX)
cleanup() {
  /bin/rm -f -- "$base_output" "$socket_output" "$socket_records" "$lsof_errors"
}
trap cleanup EXIT HUP INT TERM

if ! /usr/sbin/lsof -nP -a -p "$pid" -Fp >"$base_output" 2>"$lsof_errors"; then
  echo "Could not inspect the requested process." >&2
  exit 1
fi
if ! /usr/bin/grep -Fqx "p$pid" "$base_output"; then
  echo "Process inspection did not return the requested PID." >&2
  exit 1
fi

set +e
/usr/sbin/lsof -nP -a -p "$pid" -i -FpPnT >"$socket_output" 2>"$lsof_errors"
lsof_status=$?
set -e
case "$lsof_status" in
  0) ;;
  1)
    if [ -s "$socket_output" ]; then
      echo "Network inspection returned an incomplete socket ledger." >&2
      exit 1
    fi
    ;;
  *)
    echo "Network inspection failed." >&2
    exit 1
    ;;
esac

/usr/bin/awk '
  function flush() {
    if (name != "") print protocol "\t" name "\t" state;
    protocol = "";
    name = "";
    state = "";
  }
  /^p/ { flush(); next }
  /^f/ { flush(); next }
  /^P/ { protocol = substr($0, 2); next }
  /^n/ { name = substr($0, 2); next }
  /^TST=/ { state = substr($0, 5); next }
  END { flush() }
' "$socket_output" >"$socket_records"

allowed_ips=""
resolve_service_ips() {
  if [ -n "$allowed_ips" ]; then
    return
  fi
  allowed_ips=$(
    {
      /usr/bin/dig +short A api.openai.com
      /usr/bin/dig +short AAAA api.openai.com
    } 2>/dev/null |
      /usr/bin/awk '/^[0-9a-fA-F:.]+$/ { print }' |
      /usr/bin/sort -u
  )
  if [ -z "$allowed_ips" ]; then
    echo "Could not resolve the sole permitted remote service." >&2
    exit 1
  fi
}

violations=""
while IFS="$(printf '\t')" read -r protocol endpoint connection_state; do
  [ -n "$endpoint" ] || continue
  if [ "$protocol" != TCP ]; then
    violations="${violations}${violations:+
}${protocol:-UNKNOWN} $endpoint"
    continue
  fi

  if [ "$endpoint" = "127.0.0.1:3334" ] && [ "$connection_state" = LISTEN ]; then
    continue
  fi

  case "$endpoint" in
    *-\>*)
      local_endpoint=${endpoint%%-\>*}
      remote_endpoint=${endpoint#*-\>}
      case "$local_endpoint:$remote_endpoint" in
        127.0.0.1:*:127.0.0.1:*)
          local_port=${local_endpoint##*:}
          remote_port=${remote_endpoint##*:}
          case "$local_port:$remote_port" in
            *[!0-9:]* | :* | *:) ;;
            3334:* | *:3334) continue ;;
          esac
          ;;
      esac

      case "$remote_endpoint" in
        *:443)
          remote_host=${remote_endpoint%:443}
          remote_host=${remote_host#\[}
          remote_host=${remote_host%\]}
          resolve_service_ips
          if printf '%s\n' "$allowed_ips" | /usr/bin/grep -Fqx "$remote_host"; then
            continue
          fi
          ;;
      esac
      ;;
  esac

  violations="${violations}${violations:+
}TCP $endpoint"
done <"$socket_records"

if [ -n "$violations" ]; then
  echo "Unexpected woof network sockets:" >&2
  printf '%s\n' "$violations" >&2
  exit 1
fi

command_after=$(/bin/ps -p "$pid" -o command= 2>/dev/null)
if [ -z "$command_after" ] || [ "$command_after" != "$command_before" ]; then
  echo "The inspected process changed during network observation." >&2
  exit 1
fi

echo "Observed sockets stay within exact loopback and remote-service boundaries."
