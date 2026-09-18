#!/usr/bin/env bash
# Run a reproducible Hyperion-rs integration test against a live PulseVM SHiP
# endpoint. The completed offline XPR block-log replay is deliberately not used:
# it disables state-history output and therefore has no trace/delta stream.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
readonly COMPOSE_FILE="$REPO_ROOT/scripts/hyperion/docker-compose.yml"
readonly HOST_NETWORK_COMPOSE_FILE="$REPO_ROOT/scripts/hyperion/docker-compose.host.yml"
readonly DEFAULT_HYPERION_REF="22a377b797628f984d48cc054ba789f9ecc8c3c9"
readonly ACTION="${1:-help}"

absolute_from_repo() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s/%s\n' "$REPO_ROOT" "$1" ;;
  esac
}

STATE_DIR="$(absolute_from_repo "${PULSEVM_HYPERION_DIR:-build/hyperion-test}")"
readonly STATE_DIR
readonly SOURCE_DIR="$STATE_DIR/hyperion-rs"
readonly CONFIG_PATH="$STATE_DIR/config.toml"
readonly RUNTIME_PATH="$STATE_DIR/runtime.json"
readonly HYPERION_REF="${HYPERION_REF:-$DEFAULT_HYPERION_REF}"
readonly PROJECT_NAME="${HYPERION_COMPOSE_PROJECT:-pulsevm-hyperion-test}"
readonly API_PORT="${HYPERION_API_PORT:-7000}"
readonly ES_PORT="${HYPERION_ES_PORT:-9200}"
readonly API_URL="http://127.0.0.1:$API_PORT"
readonly ES_URL="http://127.0.0.1:$ES_PORT"

usage() {
  cat <<'EOF'
Usage: scripts/hyperion-test.sh [prepare|configure|start|verify|status|logs|stop]

Run Hyperion-rs and Elasticsearch against a live, SHiP-enabled PulseVM node.

Commands:
  prepare    Clone the pinned Hyperion-rs revision into build/.
  configure  Validate PulseVM RPC and generate Hyperion configuration.
  start      Prepare, configure, build, start, and verify the Docker stack.
  verify     Check RPC identity, indexing progress, and optional transaction.
  status     Show container state and the current Hyperion health response.
  logs       Follow indexer and API logs.
  stop       Stop containers while preserving the Elasticsearch volume.

Configuration:
  PULSEVM_RPC_URL             PulseVM JSON-RPC URL as seen from this host.
                              When omitted, discover it from the active EC2
                              five-node report.
  PULSEVM_SHIP_URL            SHiP WebSocket on this host
                              (default: ws://127.0.0.1:9090).
  PULSEVM_SYSTEM_ACCOUNT      Privileged account (default: pulse; use eosio
                              when that is the imported chain configuration).
  HYPERION_START_BLOCK        First block, or "auto" to start at the current
                              PulseVM head on the first run and resume after a
                              successful verification (default: auto).
  HYPERION_TEST_TX_ID         Optional transaction that must be returned by
                              /v2/history/get_transaction.
  HYPERION_TEST_ACCOUNT       Optional account whose get_actions endpoint is
                              exercised after the base verification.
  HYPERION_MAX_HEAD_LAG       Maximum indexed-block lag (default: 20).
  PULSEVM_HYPERION_DIR        Persistent harness directory
                              (default: build/hyperion-test).
  HYPERION_API_PORT           Loopback API port (default: 7000).
  HYPERION_ES_PORT            Loopback Elasticsearch port (default: 9200).
  HYPERION_ES_HEAP            Elasticsearch min/max heap (default: 1g).
  HYPERION_DOCKER_NETWORK_MODE
                              "host", "bridge", or "auto" (default). Auto
                              uses host networking on Linux so containers can
                              reach a loopback-only MetalGo RPC listener.
  HYPERION_REF                Hyperion-rs commit or ref (defaults to the
                              reviewed revision pinned by this script).

The stack binds Elasticsearch and the Hyperion API to loopback only. `stop`
does not delete indexed data. The pinned source checkout, generated config,
runtime metadata, and Docker volume are all reusable.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "missing command '$1'"
}

validate_uint() {
  local name="$1" value="$2"
  [[ "$value" =~ ^[0-9]+$ ]] || fail "$name must be an unsigned integer"
}

docker_network_mode() {
  local mode="${HYPERION_DOCKER_NETWORK_MODE:-auto}"
  case "$mode" in
    auto)
      if [[ "$(uname -s)" == "Linux" ]]; then
        printf 'host\n'
      else
        printf 'bridge\n'
      fi
      ;;
    host|bridge) printf '%s\n' "$mode" ;;
    *) fail "HYPERION_DOCKER_NETWORK_MODE must be auto, host, or bridge" ;;
  esac
}

compose() {
  local -a compose_files=(--file "$COMPOSE_FILE")
  if [[ "$(docker_network_mode)" == "host" ]]; then
    compose_files+=(--file "$HOST_NETWORK_COMPOSE_FILE")
  fi
  HYPERION_SOURCE_DIR="$SOURCE_DIR" \
  HYPERION_CONFIG_PATH="$CONFIG_PATH" \
  HYPERION_API_PORT="$API_PORT" \
  HYPERION_ES_PORT="$ES_PORT" \
  HYPERION_ES_HEAP="${HYPERION_ES_HEAP:-1g}" \
    docker compose --project-name "$PROJECT_NAME" "${compose_files[@]}" "$@"
}

discover_rpc_url() {
  if [[ -n "${PULSEVM_RPC_URL:-}" ]]; then
    printf '%s\n' "$PULSEVM_RPC_URL"
    return
  fi

  local locator="$REPO_ROOT/build/xpr-mainnet-ec2-run-root"
  local run_root session report rpc
  [[ -s "$locator" ]] || fail "set PULSEVM_RPC_URL; no active EC2 run locator was found"
  run_root="$(<"$locator")"
  case "$run_root" in
    /*) ;;
    *) run_root="$REPO_ROOT/$run_root" ;;
  esac
  [[ -s "$run_root/current-session" ]] || fail "set PULSEVM_RPC_URL; EC2 session metadata is missing"
  session="$(<"$run_root/current-session")"
  report="$session/five-node-replay.json"
  [[ -s "$report" ]] || fail "set PULSEVM_RPC_URL; five-node report is missing: $report"
  rpc="$(jq -er '.nodes[0].rpc | select(type == "string" and length > 0)' "$report")"
  printf '%s\n' "$rpc"
}

docker_host_url() {
  local value="$1"
  if [[ "$value" =~ ^(https?|wss?)://(127\.0\.0\.1|localhost)(.*)$ ]]; then
    value="${BASH_REMATCH[1]}://host.docker.internal${BASH_REMATCH[3]}"
  elif [[ "$value" =~ ^(https?|wss?)://\[::1\](.*)$ ]]; then
    value="${BASH_REMATCH[1]}://host.docker.internal${BASH_REMATCH[2]}"
  fi
  printf '%s\n' "$value"
}

validate_url_value() {
  local name="$1" value="$2" schemes="$3"
  case "$schemes:$value" in
    http:http://*|http:https://*|ws:ws://*|ws:wss://*) ;;
    http:*) fail "$name must use http:// or https://" ;;
    ws:*) fail "$name must use ws:// or wss://" ;;
  esac
  [[ "$value" != *$'\n'* && "$value" != *$'\r'* && "$value" != *'"'* ]] || \
    fail "$name contains characters that cannot be written safely to TOML"
}

pulse_info() {
  local rpc_url="$1"
  curl --fail --silent --show-error --max-time "${PULSEVM_RPC_TIMEOUT:-10}" \
    --request POST "$rpc_url" \
    --header 'content-type: application/json' \
    --data '{"jsonrpc":"2.0","id":1,"method":"pulsevm.getInfo","params":[]}'
}

prepare_source() {
  require_command git
  mkdir -p "$STATE_DIR"
  if [[ ! -e "$SOURCE_DIR" ]]; then
    echo "==> Cloning Hyperion-rs"
    git clone https://github.com/MetalBlockchain/hyperion-rs.git "$SOURCE_DIR"
  fi
  [[ -d "$SOURCE_DIR/.git" ]] || fail "$SOURCE_DIR exists but is not a Git checkout"
  if ! git -C "$SOURCE_DIR" cat-file -e "$HYPERION_REF^{commit}" 2>/dev/null; then
    echo "==> Fetching pinned Hyperion-rs revision $HYPERION_REF"
    git -C "$SOURCE_DIR" fetch origin "$HYPERION_REF"
  fi
  if [[ -n "$(git -C "$SOURCE_DIR" status --porcelain)" ]]; then
    fail "Hyperion checkout has local changes: $SOURCE_DIR"
  fi
  git -C "$SOURCE_DIR" checkout --detach --quiet "$HYPERION_REF"
  local actual expected
  actual="$(git -C "$SOURCE_DIR" rev-parse HEAD)"
  expected="$(git -C "$SOURCE_DIR" rev-parse "$HYPERION_REF^{commit}")"
  [[ "$actual" == "$expected" ]] || fail "Hyperion revision mismatch: $actual"
  echo "Hyperion-rs revision: $actual"
}

configure() {
  require_command curl
  require_command jq
  mkdir -p "$STATE_DIR"

  local rpc_url ship_url docker_rpc_url docker_ship_url elasticsearch_url api_listen
  local network_mode info chain_id head start_block
  local chain_name system_account config_tmp runtime_tmp old_chain old_resume
  rpc_url="$(discover_rpc_url)"
  ship_url="${PULSEVM_SHIP_URL:-ws://127.0.0.1:9090}"
  validate_url_value PULSEVM_RPC_URL "$rpc_url" http
  validate_url_value PULSEVM_SHIP_URL "$ship_url" ws

  info="$(pulse_info "$rpc_url")"
  if jq -e '.error != null' >/dev/null <<<"$info"; then
    jq '.error' <<<"$info" >&2
    fail "PulseVM RPC returned an error"
  fi
  chain_id="$(jq -er '.result.chain_id | select(type == "string" and length > 0)' <<<"$info")"
  head="$(jq -er '.result.head_block_num | select(type == "number")' <<<"$info")"
  validate_uint head_block_num "$head"

  old_chain=""
  old_resume="false"
  if [[ -s "$RUNTIME_PATH" ]]; then
    old_chain="$(jq -r '.chain_id // empty' "$RUNTIME_PATH")"
    old_resume="$(jq -r '.resume_enabled // false' "$RUNTIME_PATH")"
    if [[ -n "$old_chain" && "$old_chain" != "$chain_id" ]]; then
      fail "state directory belongs to chain $old_chain, not $chain_id; choose a new PULSEVM_HYPERION_DIR"
    fi
  fi

  start_block="${HYPERION_START_BLOCK:-auto}"
  if [[ "$start_block" == "auto" ]]; then
    if [[ "$old_resume" == "true" ]]; then
      start_block=0
    else
      start_block="$head"
    fi
  fi
  validate_uint HYPERION_START_BLOCK "$start_block"

  chain_name="${HYPERION_CHAIN_NAME:-pulsevm-${chain_id:0:12}}"
  [[ "$chain_name" =~ ^[a-z0-9][a-z0-9_-]*$ ]] || \
    fail "HYPERION_CHAIN_NAME must match [a-z0-9][a-z0-9_-]*"
  system_account="${PULSEVM_SYSTEM_ACCOUNT:-pulse}"
  [[ "$system_account" =~ ^[.a-z1-5]{1,13}$ ]] || fail "invalid PULSEVM_SYSTEM_ACCOUNT"
  validate_uint HYPERION_MAX_MESSAGES_IN_FLIGHT "${HYPERION_MAX_MESSAGES_IN_FLIGHT:-128}"
  validate_uint HYPERION_BATCH_SIZE "${HYPERION_BATCH_SIZE:-2000}"
  validate_uint HYPERION_FLUSH_INTERVAL_MS "${HYPERION_FLUSH_INTERVAL_MS:-500}"
  (( ${HYPERION_MAX_MESSAGES_IN_FLIGHT:-128} > 0 )) || fail "HYPERION_MAX_MESSAGES_IN_FLIGHT must be positive"
  (( ${HYPERION_BATCH_SIZE:-2000} > 0 )) || fail "HYPERION_BATCH_SIZE must be positive"

  network_mode="$(docker_network_mode)"
  if [[ "$network_mode" == "host" ]]; then
    docker_rpc_url="$rpc_url"
    docker_ship_url="$ship_url"
    elasticsearch_url="http://127.0.0.1:$ES_PORT"
    api_listen="127.0.0.1:$API_PORT"
  else
    docker_rpc_url="$(docker_host_url "$rpc_url")"
    docker_ship_url="$(docker_host_url "$ship_url")"
    elasticsearch_url="http://elasticsearch:9200"
    api_listen="0.0.0.0:7000"
  fi
  config_tmp="$(mktemp "$STATE_DIR/config.toml.XXXXXX")"
  runtime_tmp="$(mktemp "$STATE_DIR/runtime.json.XXXXXX")"
  trap 'rm -f -- "${config_tmp:-}" "${runtime_tmp:-}"' RETURN

  {
    printf '[chain]\n'
    printf 'name = "%s"\n' "$chain_name"
    printf 'http = "%s"\n' "$docker_rpc_url"
    printf 'ship = "%s"\n' "$docker_ship_url"
    printf 'api = "pulsevm"\n'
    printf 'system_account = "%s"\n\n' "$system_account"
    printf '[indexer]\n'
    printf 'start_block = %s\n' "$start_block"
    printf 'stop_block = 0\n'
    printf 'fetch_block = true\n'
    printf 'fetch_traces = true\n'
    printf 'fetch_deltas = true\n'
    printf 'max_messages_in_flight = %s\n' "${HYPERION_MAX_MESSAGES_IN_FLIGHT:-128}"
    printf 'batch_size = %s\n' "${HYPERION_BATCH_SIZE:-2000}"
    printf 'flush_interval_ms = %s\n' "${HYPERION_FLUSH_INTERVAL_MS:-500}"
    printf 'skip_actions = []\n\n'
    printf '[elasticsearch]\n'
    printf 'url = "%s"\n' "$elasticsearch_url"
    printf 'user = ""\n'
    printf 'pass = ""\n'
    printf 'shards = 1\n'
    printf 'replicas = 0\n\n'
    printf '[api]\n'
    printf 'listen = "%s"\n' "$api_listen"
    printf 'max_limit = 1000\n'
  } >"$config_tmp"
  mv "$config_tmp" "$CONFIG_PATH"

  jq -n \
    --arg rpc_url "$rpc_url" \
    --arg ship_url "$ship_url" \
    --arg chain_id "$chain_id" \
    --arg chain_name "$chain_name" \
    --arg system_account "$system_account" \
    --arg docker_network_mode "$network_mode" \
    --arg hyperion_ref "$HYPERION_REF" \
    --argjson configured_head "$head" \
    --argjson start_block "$start_block" \
    --argjson resume_enabled "$old_resume" \
    '{rpc_url:$rpc_url,ship_url:$ship_url,chain_id:$chain_id,chain_name:$chain_name,system_account:$system_account,docker_network_mode:$docker_network_mode,configured_head:$configured_head,start_block:$start_block,hyperion_ref:$hyperion_ref,resume_enabled:$resume_enabled}' \
    >"$runtime_tmp"
  mv "$runtime_tmp" "$RUNTIME_PATH"
  trap - RETURN

  echo "Hyperion config: $CONFIG_PATH"
  echo "PulseVM chain:   $chain_id"
  echo "PulseVM head:    $head"
  echo "Index from:      $start_block"
  echo "PulseVM SHiP:    $ship_url"
  echo "Docker network:  $network_mode"
}

enable_resume() {
  local head="$1" indexed="$2" lag="$3" block_count="$4" tx_id="$5"
  local config_tmp runtime_tmp
  config_tmp="$(mktemp "$STATE_DIR/config.toml.XXXXXX")"
  runtime_tmp="$(mktemp "$STATE_DIR/runtime.json.XXXXXX")"
  trap 'rm -f -- "${config_tmp:-}" "${runtime_tmp:-}"' RETURN
  sed -E 's/^start_block = [0-9]+$/start_block = 0/' "$CONFIG_PATH" >"$config_tmp"
  grep -qx 'start_block = 0' "$config_tmp" || fail "could not enable Hyperion resume mode"
  jq \
    --arg verified_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg transaction_id "$tx_id" \
    --argjson head "$head" \
    --argjson indexed "$indexed" \
    --argjson lag "$lag" \
    --argjson block_count "$block_count" \
    '.start_block = 0 |
     .resume_enabled = true |
     .verification = {
       status: "passed",
       verified_at: $verified_at,
       head_block_num: $head,
       last_indexed_block: $indexed,
       head_lag: $lag,
       indexed_blocks: $block_count,
       transaction_id: (if $transaction_id == "" then null else $transaction_id end)
     }' \
    "$RUNTIME_PATH" >"$runtime_tmp"
  mv "$config_tmp" "$CONFIG_PATH"
  mv "$runtime_tmp" "$RUNTIME_PATH"
  trap - RETURN
}

wait_for_api() {
  local timeout="${HYPERION_START_TIMEOUT_SECONDS:-300}"
  validate_uint HYPERION_START_TIMEOUT_SECONDS "$timeout"
  ((timeout > 0)) || fail "HYPERION_START_TIMEOUT_SECONDS must be positive"
  local deadline=$((SECONDS + timeout))
  while ((SECONDS < deadline)); do
    if curl --fail --silent --max-time 3 "$API_URL/v2/health" >/dev/null 2>&1; then
      return
    fi
    sleep 2
  done
  compose logs --tail=100 indexer api >&2 || true
  fail "Hyperion API did not become ready within ${timeout}s"
}

index_count() {
  local index="$1"
  curl --fail --silent --show-error --max-time 10 "$ES_URL/$index/_count" | jq -er '.count'
}

verify() {
  require_command curl
  require_command jq
  [[ -s "$RUNTIME_PATH" ]] || fail "runtime metadata is missing; run configure or start"

  local rpc_url expected_chain chain_name max_lag timeout deadline info health
  local actual_chain head indexed lag block_count tx_id tx_id_lower account tx_result verified
  rpc_url="$(jq -er '.rpc_url' "$RUNTIME_PATH")"
  expected_chain="$(jq -er '.chain_id' "$RUNTIME_PATH")"
  chain_name="$(jq -er '.chain_name' "$RUNTIME_PATH")"
  max_lag="${HYPERION_MAX_HEAD_LAG:-20}"
  timeout="${HYPERION_VERIFY_TIMEOUT_SECONDS:-180}"
  validate_uint HYPERION_MAX_HEAD_LAG "$max_lag"
  validate_uint HYPERION_VERIFY_TIMEOUT_SECONDS "$timeout"
  ((timeout > 0)) || fail "HYPERION_VERIFY_TIMEOUT_SECONDS must be positive"

  deadline=$((SECONDS + timeout))
  verified=false
  while ((SECONDS < deadline)); do
    info="$(pulse_info "$rpc_url")"
    actual_chain="$(jq -er '.result.chain_id' <<<"$info")"
    [[ "$actual_chain" == "$expected_chain" ]] || \
      fail "PulseVM chain changed from $expected_chain to $actual_chain"
    head="$(jq -er '.result.head_block_num' <<<"$info")"
    health="$(curl --fail --silent --show-error --max-time 10 "$API_URL/v2/health")"
    if jq -e --arg chain_id "$expected_chain" '
      any(.health[]; .service == "Elasticsearch" and .status == "OK") and
      any(.health[];
        .service == "PulseVM-RPC" and
        .status == "OK" and
        .service_data.chain_id == $chain_id
      )
    ' >/dev/null <<<"$health"; then
      indexed="$(jq -er '.health[] | select(.service == "Indexer") | .service_data.last_indexed_block' <<<"$health")"
      if ((indexed <= head)); then
        lag=$((head - indexed))
        if ((indexed > 0 && lag <= max_lag)); then
          verified=true
          break
        fi
      fi
    fi
    sleep 2
  done
  if [[ "$verified" != "true" ]]; then
    jq . <<<"${health:-{}}" >&2 || true
    compose logs --tail=100 indexer api >&2 || true
    fail "Hyperion did not index within $max_lag blocks of PulseVM in ${timeout}s"
  fi

  block_count="$(index_count "$chain_name-block")"
  ((block_count > 0)) || fail "Hyperion block index is empty"

  tx_id="${HYPERION_TEST_TX_ID:-}"
  if [[ -n "$tx_id" ]]; then
    [[ "$tx_id" =~ ^[[:xdigit:]]{64}$ ]] || fail "HYPERION_TEST_TX_ID must be a 64-digit hex value"
    tx_id_lower="$(printf '%s' "$tx_id" | tr '[:upper:]' '[:lower:]')"
    tx_result="$(curl --fail --silent --show-error --get --max-time 10 \
      --data-urlencode "id=$tx_id" "$API_URL/v2/history/get_transaction")"
    jq -e --arg id "$tx_id_lower" '.executed == true and (.trx_id | ascii_downcase) == $id and (.actions | length > 0)' \
      >/dev/null <<<"$tx_result" || fail "Hyperion did not return transaction $tx_id with actions"
  fi

  account="${HYPERION_TEST_ACCOUNT:-}"
  if [[ -n "$account" ]]; then
    curl --fail --silent --show-error --get --max-time 10 \
      --data-urlencode "account=$account" \
      --data-urlencode 'limit=10' \
      "$API_URL/v2/history/get_actions" | \
      jq -e '.actions | type == "array"' >/dev/null || fail "get_actions failed for $account"
  fi

  echo "Hyperion integration passed: chain=$expected_chain head=$head indexed=$indexed lag=$lag blocks=$block_count"
  enable_resume "$head" "$indexed" "$lag" "$block_count" "$tx_id"
  echo "Hyperion resume mode enabled for subsequent starts"
  if [[ -z "$tx_id" ]]; then
    echo "note: set HYPERION_TEST_TX_ID to add action/ABI verification for a known transaction"
  fi
}

start() {
  require_command docker
  docker compose version >/dev/null
  prepare_source
  configure
  echo "==> Building and starting Elasticsearch, Hyperion indexer, and API"
  compose up --detach --build
  wait_for_api
  verify
}

status() {
  require_command docker
  require_command curl
  require_command jq
  compose ps
  if curl --fail --silent --max-time 5 "$API_URL/v2/health" >/dev/null 2>&1; then
    curl --fail --silent --show-error --max-time 10 "$API_URL/v2/health" | jq .
  fi
}

case "$ACTION" in
  prepare) prepare_source ;;
  configure) configure ;;
  start) start ;;
  verify) verify ;;
  status) status ;;
  logs) compose logs --follow indexer api ;;
  stop) compose down ;;
  -h|--help|help) usage ;;
  *) usage >&2; exit 2 ;;
esac
