#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf '%s\n' "Usage: $0 [--profile PATH] [--only LAYER[,LAYER...]] [--list] [--dry-run]" \
    "  --profile PATH  command profile relative to the project root" \
    "  --only LAYERS   run only the selected layers" \
    "  --list          print the profile without running checks" \
    "  --dry-run       print selected checks without running them"
}

fail() {
  printf 'verify-project: %s\n' "$*" >&2
  exit 2
}

PROFILE_PATH="scripts/verification-profile.tsv"
ONLY=""
LIST_ONLY=0
DRY_RUN=0
seen_names=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --profile)
      [ "$#" -ge 2 ] || fail "--profile requires a path"
      PROFILE_PATH="$2"
      shift 2
      ;;
    --only)
      [ "$#" -ge 2 ] || fail "--only requires a layer list"
      ONLY="$2"
      shift 2
      ;;
    --list)
      LIST_ONLY=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown option: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

case "$PROFILE_PATH" in
  /*) PROFILE_FILE="$PROFILE_PATH" ;;
  *) PROFILE_FILE="$PROJECT_ROOT/$PROFILE_PATH" ;;
esac

valid_layer() {
  case " static positive negative security regression integration build " in
    *" $1 "*) return 0 ;;
    *) return 1 ;;
  esac
}

selected_layer() {
  [ -z "$ONLY" ] && return 0
  case ",$ONLY," in
    *",$1,"*) return 0 ;;
    *) return 1 ;;
  esac
}

if [ -n "$ONLY" ]; then
  case "$ONLY" in
    ,*|*,|*,,*) fail "empty layer in --only" ;;
  esac
  IFS=',' read -r -a requested_layers <<<"$ONLY"
  [ "${#requested_layers[@]}" -gt 0 ] || fail "empty --only list"
  for layer in "${requested_layers[@]}"; do
    valid_layer "$layer" || fail "unknown layer: $layer"
  done
fi

[ -f "$PROFILE_FILE" ] || fail "profile not found: $PROFILE_FILE"
[ -r "$PROFILE_FILE" ] || fail "profile is not readable: $PROFILE_FILE"

cd "$PROJECT_ROOT"

found=0
selected=0
failures=0
warnings=0
line_number=0

while IFS=$'\t' read -r name layer required command extra || [ -n "${name:-}${layer:-}${required:-}${command:-}${extra:-}" ]; do
  line_number=$((line_number + 1))
  name="${name%$'\r'}"
  layer="${layer%$'\r'}"
  required="${required%$'\r'}"
  command="${command%$'\r'}"
  extra="${extra%$'\r'}"

  [ -n "$name" ] || continue
  case "$name" in
    \#*) continue ;;
  esac

  [ -z "$extra" ] || fail "line $line_number: extra fields"
  [[ "$name" =~ ^[A-Za-z0-9._-]+$ ]] || fail "line $line_number: invalid check name"
  valid_layer "$layer" || fail "line $line_number: unknown layer '$layer'"
  case "$required" in
    0|1) ;;
    *) fail "line $line_number: required must be 0 or 1" ;;
  esac
  [ -n "$command" ] || fail "line $line_number: command cannot be empty"
  case ";$seen_names;" in
    *";$name;"*) fail "line $line_number: duplicate check name '$name'" ;;
  esac
  seen_names="$seen_names;$name"
  found=1

  if [ "$LIST_ONLY" -eq 1 ]; then
    if selected_layer "$layer"; then
      printf '%s\t%s\t%s\t%s\n' "$name" "$layer" "$required" "$command"
    else
      printf '%s\t%s\t%s\t%s\tskipped\n' "$name" "$layer" "$required" "$command"
    fi
    continue
  fi

  selected_layer "$layer" || continue
  selected=1
  printf '[verify] %s (%s)\n' "$name" "$layer"

  if [ "$DRY_RUN" -eq 1 ]; then
    printf '[verify] dry-run: %s\n' "$command"
    continue
  fi

  if bash -c "$command"; then
    printf '[verify] %s: passed\n' "$name"
  else
    status=$?
    if [ "$required" -eq 1 ]; then
      failures=$((failures + 1))
      printf '[verify] %s: failed (exit %s, required)\n' "$name" "$status" >&2
    else
      warnings=$((warnings + 1))
      printf '[verify] %s: warning (exit %s, optional)\n' "$name" "$status" >&2
    fi
  fi
done <"$PROFILE_FILE"

[ "$found" -eq 1 ] || fail "profile contains no checks"

if [ "$LIST_ONLY" -eq 1 ]; then
  exit 0
fi

[ "$selected" -eq 1 ] || fail "no checks selected to run"

if [ "$failures" -gt 0 ]; then
  printf '[verify] failed: %s required check(s), %s warning(s)\n' "$failures" "$warnings" >&2
  exit 1
fi

printf '[verify] passed: %s required failure(s), %s warning(s)\n' "$failures" "$warnings"
