# `@infinity-msfs/instrument`

Pure ReScript API for registering MSFS `BaseInstrument` subclasses.

ReScript cannot emit ES6 `class extends` syntax with working `super` calls, which is the one thing `BaseInstrument` requires. This library isolates that concern into a ~50-line JS shim (`instrumentShell.mjs`) behind a small typed ReScript API. Everything else about your gauge stays in ReScript.

## Usage

```rescript
let relay: ref<option<MyRelay.t>> = ref(None)

Instrument.define(
  "my-gauge-instrument",
  {
    templateID: "MyGaugeTemplate",
    onConnected: _self => {
      let r = MyRelay.make()
      MyRelay.init(r)
      relay := Some(r)
    },
    onDisconnected: _self => {
      relay.contents->Option.forEach(MyRelay.destroy)
      relay := None
    },
  },
)
```

## API

### `type instance`

Opaque handle for the `BaseInstrument` `this` reference. Treat it as a
key for per-instance state (e.g. in a `WeakMap`).

### `type spec`

```rescript
type spec = {
  templateID: string,
  onConnected?: instance => unit,
  onDisconnected?: instance => unit,
  onUpdate?: instance => unit,
}
```

- `templateID` — the HTML template identifier MSFS looks up.
- `onConnected` — runs after `super.connectedCallback()`.
- `onDisconnected` — runs before `super.disconnectedCallback()`.
- `onUpdate` — runs after `super.Update()` every sim tick.

### `Instrument.define(name, spec): unit`

Registers the subclass with MSFS. Call once at module load.
