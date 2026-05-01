# `@infinity-msfs/rescript-bridge-relay`

ReScript implementation of the MSFS gauge relay — bridges a WebSocket connection from the host application to the `CommBus` of the adjacent WASM gauge.

Drop-in equivalent of [`@infinity-msfs/ts-bridge-relay`](https://www.npmjs.com/package/@infinity-msfs/ts-bridge-relay) (TypeScript) for ReScript projects.

## Install

```sh
npm install @infinity-msfs/rescript-bridge-relay
```

Then add to `rescript.json`:

```json
{
  "bs-dependencies": ["@rescript/core", "@infinity-msfs/rescript-bridge-relay"]
}
```

## Usage

```rescript
let relay = BridgeRelay.make({
  wsUrl: "ws://127.0.0.1:9876/bridge",
  callEvent: "myaddon/bridge_call",
  responseEvent: "myaddon/bridge_resp",
  hello: Some({client: Some("msfs-gauge"), aircraft: Some("DC-10-30")}),
})

BridgeRelay.init(relay)
```

See the [repository README](https://github.com/infinity-MSFS/infinity-wasm-bridge#readme) for the full architecture and host-side setup.

## License

MIT
