// Public API for defining MSFS BaseInstrument subclasses from ReScript.
//
// Consumers call `Instrument.define` with a spec record. The spec's
// callbacks receive the raw instance, which is treated as an opaque
// handle — you don't read fields off it from ReScript, you just use
// it as a key for per-instance state (typically a WeakMap or a ref).

// Opaque handle for the BaseInstrument `this` reference.
type instance

// Lifecycle spec. All hooks are optional except templateID.
//
// - onConnected runs AFTER super.connectedCallback(). This matches what
//   you'd naturally write in a TypeScript subclass.
// - onDisconnected runs BEFORE super.disconnectedCallback() so user
//   cleanup finishes before the parent class tears itself down.
// - onUpdate runs AFTER super.Update() every tick.
type spec = {
  templateID: string,
  onConnected?: instance => unit,
  onDisconnected?: instance => unit,
  onUpdate?: instance => unit,
}

// Register the subclass with MSFS. Call this once at module load.
@module("./instrumentShell.mjs")
external define: (string, spec) => unit = "defineInstrument"
