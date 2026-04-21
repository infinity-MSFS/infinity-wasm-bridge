// FFI bindings to the MSFS JS runtime and browser globals used by the
// relay. Kept deliberately minimal — only what BridgeRelay.res touches.
//
// If you find yourself reaching for another MSFS API, add a binding
// here rather than using %raw at the call site.

// ---- ViewListener (CommBus) ----
//
// The handle returned by RegisterViewListener. Opaque from ReScript —
// we only interact with it through the three methods below.

type viewListener

@send external on_: (viewListener, string, string => unit) => unit = "on"
@send external off: (viewListener, string, string => unit) => unit = "off"
@send external unregister: viewListener => unit = "unregister"

// The CommBus call signature per MSFS conventions:
//   call(callback_name, event_name, json_payload) -> Promise<any>
@send
external call: (viewListener, string, string, string) => promise<JSON.t> = "call"

@val
external registerViewListener: (string, unit => unit) => viewListener =
  "RegisterViewListener"

// ---- Browser timers ----
//
// The actual return type of setTimeout varies by runtime (number in
// browsers, Timeout in Node). We treat it as opaque and only feed it
// back to clearTimeout. This avoids pinning the binding to one target.

type timerId

@val external setTimeout: (unit => unit, int) => timerId = "setTimeout"
@val external clearTimeout: timerId => unit = "clearTimeout"

// ---- WebSocket ----
//
// We bind only the bits the relay touches. readyState constants are
// frozen in the WebSocket spec, so hard-coding them is safe and saves
// a binding per constant.

type webSocket

@new external makeWebSocket: string => webSocket = "WebSocket"
@send external wsSend: (webSocket, string) => unit = "send"
@send external wsClose: webSocket => unit = "close"

@set external setOnOpen: (webSocket, unit => unit) => unit = "onopen"

// MessageEvent has a `data` field whose type depends on the server. For
// JSON relays it's always a string, but we type it as JSON.t and classify
// at the handler to stay honest.
type messageEvent = {data: JSON.t}
@set
external setOnMessage: (webSocket, messageEvent => unit) => unit = "onmessage"

@set external setOnError: (webSocket, unit => unit) => unit = "onerror"
@set external setOnClose: (webSocket, unit => unit) => unit = "onclose"

@get external readyState: webSocket => int = "readyState"

let wsConnecting = 0
let wsOpen = 1
let wsClosing = 2
let wsClosed = 3
