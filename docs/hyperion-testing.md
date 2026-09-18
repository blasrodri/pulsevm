# Hyperion integration testing

PulseVM exposes an Antelope state-history (SHiP) WebSocket service that can be
consumed by [Hyperion-rs](https://github.com/MetalBlockchain/hyperion-rs). This
test verifies the live data path:

```text
PulseVM block execution -> SHiP traces/deltas -> Hyperion-rs -> Elasticsearch -> Hyperion API
```

It does **not** turn an existing offline XPR replay into historical Hyperion
data. `xpr_blocklog_replay` deliberately sets `state_history_enabled` to false,
so its Arena directory has canonical final state but no historical SHiP trace
or chain-state logs.

## Prerequisites

- A live PulseVM node with `state_history_enabled: true`. `scripts/run-local.sh`
  enables this explicitly by default.
- The node's PulseVM JSON-RPC URL.
- The node's SHiP WebSocket endpoint. The first co-located VM normally binds
  `0.0.0.0:9090`; subsequent VMs use ephemeral ports. Confirm it in the node
  logs by searching for `WebSocket listening`.
- Docker with Compose, `curl`, `git`, and `jq`.

Keep ports 7000, 9090, and 9200 private. The harness publishes Hyperion and
Elasticsearch on loopback only; use an SSH tunnel when running it on EC2.

For a public progress view, run `scripts/hyperion-status-server.py` on loopback
and point Cloudflare Tunnel at its port. The status server exposes only block
heights, lag, health, and the last verification result; it does not proxy the
PulseVM, SHiP, Hyperion, or Elasticsearch interfaces.

## EC2 migration network

Start the five-node network normally. State history is enabled for the new
PulseVM blocks authored after the imported migration boundary:

```sh
PULSEVM_STATE_HISTORY_ENABLED=true \
  scripts/run-xpr-mainnet-ec2.sh start
```

Then start Hyperion from the same checkout:

```sh
scripts/hyperion-test.sh start
```

The harness discovers the first RPC URL from the active five-node report and
uses `ws://127.0.0.1:9090` for SHiP. Set the endpoints explicitly when automatic
discovery is unavailable:

```sh
PULSEVM_RPC_URL=http://127.0.0.1:9650/ext/bc/CHAIN_ID/rpc \
PULSEVM_SHIP_URL=ws://127.0.0.1:9090 \
PULSEVM_SYSTEM_ACCOUNT=pulse \
  scripts/hyperion-test.sh start
```

On Linux, the harness automatically gives the Hyperion indexer and API host
network access. This lets them reach MetalGo's loopback-only RPC listener
without making that listener public. Elasticsearch and the Hyperion API remain
bound to loopback. On Docker Desktop the default is bridge networking through
`host.docker.internal`. Override automatic selection with
`HYPERION_DOCKER_NETWORK_MODE=host` or `bridge` when needed.

For imported XPR configuration that uses `eosio` as its privileged account,
set `PULSEVM_SYSTEM_ACCOUNT=eosio`. This must match the PulseVM node config so
that Hyperion recognizes system `setabi` actions correctly.

On the first run, indexing starts at the current PulseVM head. This is
intentional: an imported checkpoint can begin at a high revision without
retaining earlier PulseVM block or SHiP logs. After verification succeeds, the
harness changes the generated configuration to `start_block = 0`, which makes
later starts resume after the highest indexed block. Override
`HYPERION_START_BLOCK` only with a block in the SHiP server's retained range.

The default Elasticsearch index prefix includes the first 12 digits of the
chain ID. The harness also refuses to reuse one state directory for a different
chain ID, preventing histories from being mixed accidentally.

## Verification levels

The default verification proves that:

- Hyperion can reach Elasticsearch and PulseVM's JSON-RPC API;
- the API reports the same chain ID as PulseVM;
- SHiP blocks are being indexed;
- the indexed head remains within `HYPERION_MAX_HEAD_LAG` blocks of PulseVM;
- the block index contains documents.

For the complete trace and ABI-decoding gate, submit a known transaction after
Hyperion starts and supply its ID:

```sh
HYPERION_TEST_TX_ID=0123...cdef \
HYPERION_TEST_ACCOUNT=alice \
  scripts/hyperion-test.sh verify
```

The transaction check requires `get_transaction` to return at least one decoded
action. The account check also exercises `get_actions`.

Useful lifecycle commands:

```sh
scripts/hyperion-test.sh status
scripts/hyperion-test.sh logs
scripts/hyperion-test.sh verify
scripts/hyperion-test.sh stop
```

`stop` preserves the Elasticsearch volume and generated configuration. The
Hyperion source is pinned to a reviewed commit and stored under
`build/hyperion-test/`, which is ignored by Git.

## Progress endpoint

The status server defaults to `127.0.0.1:8080` and reads the generated runtime
report. Supply the private PulseVM RPC URL in its environment:

```sh
PULSEVM_RPC_URL=http://127.0.0.1:9650/ext/bc/CHAIN_ID/rpc \
HYPERION_RUNTIME_PATH="$PWD/build/hyperion-test/runtime.json" \
  scripts/hyperion-status-server.py
```

The HTML dashboard is served at `/`, with machine-readable status at
`/api/status` and `/health`. A Cloudflare quick tunnel can publish it without
opening an EC2 security-group port:

```sh
cloudflared tunnel --no-autoupdate --url http://127.0.0.1:8080
```

For a persistent EC2 deployment, install
`scripts/hyperion/pulsevm-hyperion-status.service` under `/etc/systemd/system/`,
install the server as `/usr/local/bin/pulsevm-hyperion-status-server`, and copy
`scripts/hyperion/hyperion-status.env.example` to
`/etc/pulsevm/hyperion-status.env` with the actual chain RPC URL. The included
unit is read-only, limits network access to localhost, and restarts on failure.
Cloudflare Tunnel remains the only public-facing process.

The companion `pulsevm-hyperion-status-tunnel.service` deliberately uses a
weak `Wants=` dependency on the status service. Restarting the dashboard must
not restart an accountless Cloudflare quick tunnel because every tunnel restart
permanently changes its generated hostname.

## Full XPR history

A full historical validation requires either the original XPR/nodeos SHiP
stream or a fresh PulseVM replay that emits state-history logs. Do not enable
state history halfway through an existing replay directory: its SHiP ranges
would not cover the skipped history. Before attempting the full corpus, run a
bounded replay into an empty data directory, index it into an empty
Elasticsearch volume, and compare sampled actions, deltas, block IDs, and
document counts against the source chain.
