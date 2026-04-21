// instrumentShell.mjs
//
// Generic BaseInstrument subclass factory. ReScript cannot emit ES class
// syntax with working `super` calls, so we isolate that one concern here.
// Everything else about the gauge runtime lives in ReScript.
//
// The factory is intentionally untyped at the JS layer — the ReScript
// binding in Instrument.res is the source of truth for the spec shape
// and is what consumers interact with.
//
// `BaseInstrument` and `registerInstrument` are MSFS globals injected
// into the gauge runtime. We reference them directly.

/**
 * Define and register a BaseInstrument subclass.
 *
 * @param {string} name - The custom element tag name.
 * @param {{
 *   templateID: string,
 *   onConnected?: (self: BaseInstrument) => void,
 *   onDisconnected?: (self: BaseInstrument) => void,
 *   onUpdate?: (self: BaseInstrument) => void,
 * }} spec
 */
export function defineInstrument(name, spec) {
  // Capture spec by closure. We don't copy it into class fields because
  // ReScript's optional fields serialize with `undefined` sentinels and
  // we want simple `if (cb)` checks below.
  const onConnected = spec.onConnected;
  const onDisconnected = spec.onDisconnected;
  const onUpdate = spec.onUpdate;
  const templateID = spec.templateID;

  class Generated extends BaseInstrument {
    get templateID() {
      return templateID;
    }

    connectedCallback() {
      super.connectedCallback();
      if (onConnected) onConnected(this);
    }

    disconnectedCallback() {
      // User cleanup runs BEFORE super so the parent class tearing down
      // its event listeners doesn't race the user's disposal path.
      if (onDisconnected) onDisconnected(this);
      super.disconnectedCallback();
    }

    Update() {
      super.Update();
      if (onUpdate) onUpdate(this);
    }
  }

  registerInstrument(name, Generated);
}
