// BridgeRelay — bidirectional bridge between a desktop host over
// WebSocket and a WASM gauge over MSFS CommBus.
//
// Message flow:
//
//   Host  ─(WebSocket)─>  Relay  ─(CommBus)─>  WASM gauge
//                           │                      │
//                           └────── resp ──────────┘
//                                    │
//                             (as ack to host)
//
// The relay owns two concerns:
//  1. Bidirectional forwarding with request/response correlation.
//  2. WebSocket connection lifecycle with exponential-backoff reconnect.

open MsfsBindings

// ---- Pending requests ----

type pendingRequest = {
  resolve: JSON.t => unit,
  reject: exn => unit,
  timerId: timerId,
}

// ---- Config ----
//
// The public `config` type uses optional fields for defaults; `resolvedConfig`
// is what we store internally after defaults are applied.

type helloConfig = {
  client?: string,
  aircraft?: string,
  tail?: string,
  session?: string,
  meta?: JSON.t,
}

type config = {
  wsUrl: string,
  callEvent: string,
  responseEvent: string,
  hello?: helloConfig,
  dedupCapacity?: int,
  maxReconnectMs?: int,
  baseReconnectMs?: int,
  protocolVersion?: int,
  requestTimeoutMs?: int,
}

type resolvedConfig = {
  wsUrl: string,
  callEvent: string,
  responseEvent: string,
  hello: helloConfig,
  dedupCapacity: int,
  maxReconnectMs: int,
  baseReconnectMs: int,
  protocolVersion: int,
  requestTimeoutMs: int,
}

// ---- Relay state ----

type t = {
  config: resolvedConfig,
  mutable commBus: option<viewListener>,
  mutable ws: option<webSocket>,
  mutable wsRetryCount: int,
  mutable wsRetryTimerId: option<timerId>,
  pending: Map.t<string, pendingRequest>,
  mutable requestSeq: int,
  dedup: Dedup.t,
  // Stable reference to the WASM message handler — CommBus needs the
  // same function identity to register AND unregister the listener.
  mutable onWasmHandler: option<string => unit>,
}

let resolveConfig = (c: config): resolvedConfig => {
  wsUrl: c.wsUrl,
  callEvent: c.callEvent,
  responseEvent: c.responseEvent,
  hello: c.hello->Option.getOr({client: "msfs-gauge"}),
  dedupCapacity: c.dedupCapacity->Option.getOr(128),
  maxReconnectMs: c.maxReconnectMs->Option.getOr(30_000),
  baseReconnectMs: c.baseReconnectMs->Option.getOr(250),
  protocolVersion: c.protocolVersion->Option.getOr(1),
  requestTimeoutMs: c.requestTimeoutMs->Option.getOr(5000),
}

let make = (config: config): t => {
  let resolved = resolveConfig(config)
  {
    config: resolved,
    commBus: None,
    ws: None,
    wsRetryCount: 0,
    wsRetryTimerId: None,
    pending: Map.make(),
    requestSeq: 0,
    dedup: Dedup.make(~capacity=resolved.dedupCapacity),
    onWasmHandler: None,
  }
}

// ---- Internal helpers ----

let nextRequestId = (relay: t): string => {
  relay.requestSeq = relay.requestSeq + 1
  `${Date.now()->Float.toString}-${relay.requestSeq->Int.toString}`
}

let wsSend = (relay: t, text: string): unit =>
  switch relay.ws {
  | Some(ws) when readyState(ws) === wsOpen =>
    try MsfsBindings.wsSend(ws, text) catch {
    | exn => Console.error2("[msfs-bridge] WebSocket send failed:", exn)
    }
  | _ => ()
  }

let backoff = (relay: t, attempt: int): int => {
  let capped = attempt < 10 ? attempt : 10
  let base = Int.fromFloat(
    Math.min(
      Int.toFloat(relay.config.maxReconnectMs),
      Int.toFloat(relay.config.baseReconnectMs) *. Math.pow(2.0, ~exp=Int.toFloat(capped)),
    ),
  )
  base + Int.fromFloat(Math.floor(Math.random() *. 250.0))
}

// ---- Sending an ack back to the host ----
//
// Centralized so the timeout, error, and success paths share one shape.

let sendAck = (
  relay: t,
  ~id: string,
  ~ok: bool,
  ~response: option<JSON.t>=?,
  ~error: option<string>=?,
  ~duplicate: option<bool>=?,
): unit => {
  let ack: Wire.wireMsg = Ack({
    id,
    ok,
    response: ?response,
    error: ?error,
    duplicate: ?duplicate,
  })
  wsSend(relay, Wire.stringify(ack))
}

// ---- Connection lifecycle ----
//
// connectWs, scheduleWsReconnect, and onHostMessage form a mutually
// recursive group because the socket's onclose handler reschedules a
// reconnect, and the onmessage handler dispatches host commands which
// themselves may need relay state.

let rec connectWs = (relay: t): unit => {
  let alreadyActive = switch relay.ws {
  | Some(ws) =>
    let state = readyState(ws)
    state === wsOpen || state === wsConnecting
  | None => false
  }

  if !alreadyActive {
    try {
      let ws = makeWebSocket(relay.config.wsUrl)

      setOnOpen(ws, () => {
        relay.wsRetryCount = 0
        Console.log("[msfs-bridge] WebSocket connected")
        let hello: Wire.wireMsg = Hello({
          client: relay.config.hello.client->Option.getOr("msfs-gauge"),
          aircraft: ?relay.config.hello.aircraft,
          tail: ?relay.config.hello.tail,
          session: relay.config.hello.session->Option.getOr(
            Date.now()->Float.toString,
          ),
          v: relay.config.protocolVersion,
          meta: ?relay.config.hello.meta,
        })
        wsSend(relay, Wire.stringify(hello))
      })

      setOnMessage(ws, ev => onHostMessage(relay, ev.data))
      setOnError(ws, () => ())
      setOnClose(ws, () => {
        relay.ws = None
        scheduleWsReconnect(relay)
      })

      relay.ws = Some(ws)
    } catch {
    | _ =>
      relay.ws = None
      scheduleWsReconnect(relay)
    }
  }
}

and scheduleWsReconnect = (relay: t): unit =>
  switch relay.wsRetryTimerId {
  | Some(_) => ()
  | None =>
    let delay = backoff(relay, relay.wsRetryCount)
    relay.wsRetryCount = relay.wsRetryCount + 1
    Console.log(
      `[msfs-bridge] Reconnecting in ${delay->Int.toString}ms (attempt ${relay.wsRetryCount->Int.toString})`,
    )
    let timer = setTimeout(() => {
      relay.wsRetryTimerId = None
      connectWs(relay)
    }, delay)
    relay.wsRetryTimerId = Some(timer)
  }

// ---- Host → relay dispatch ----

and onHostMessage = (relay: t, data: JSON.t): unit =>
  switch JSON.Classify.classify(data) {
  | String(text) =>
    switch Wire.parseWireMsg(text) {
    | Some(Ping(_)) =>
      let pong: Wire.wireMsg = Pong({ts: Date.now()})
      wsSend(relay, Wire.stringify(pong))
    | Some(Cmd(_) as cmd) => onHostCommand(relay, cmd)
    | Some(Event(_) as ev) => onHostEvent(relay, ev)
    | Some(Hello(_) | Pong(_) | Ack(_)) => () // host-side messages we don't consume
    | None => Console.warn2("[msfs-bridge] Invalid message from host:", text)
    }
  | _ => ()
  }

// Wrap a host cmd in a CommBus envelope, dispatch to WASM, and when the
// WASM response arrives (or times out), unwrap it into an ack for the host.
and onHostCommand = (relay: t, cmd: Wire.wireMsg): unit =>
  switch cmd {
  | Cmd({id: cmdId}) =>
    if Dedup.has(relay.dedup, cmdId) {
      sendAck(
        relay,
        ~id=cmdId,
        ~ok=true,
        ~response=JSON.Encode.null,
        ~duplicate=true,
      )
    } else {
      Dedup.mark(relay.dedup, cmdId)

      let requestId = nextRequestId(relay)
      let envelope: Wire.commBusRequest = {
        requestId,
        payload: (Obj.magic(cmd): JSON.t),
      }

      let timerId = setTimeout(() => {
        Map.delete(relay.pending, requestId)->ignore
        sendAck(
          relay,
          ~id=cmdId,
          ~ok=false,
          ~error="WASM_TIMEOUT: no response from WASM gauge",
        )
      }, relay.config.requestTimeoutMs)

      Map.set(
        relay.pending,
        requestId,
        {
          resolve: response => sendAck(relay, ~id=cmdId, ~ok=true, ~response),
          reject: err => {
            let msg = switch err {
            | Exn.Error(e) => Exn.message(e)->Option.getOr("unknown")
            | _ => "unknown"
            }
            sendAck(relay, ~id=cmdId, ~ok=false, ~error=msg)
          },
          timerId,
        },
      )

      switch relay.commBus {
      | None =>
        switch Map.get(relay.pending, requestId) {
        | Some(p) =>
          Map.delete(relay.pending, requestId)->ignore
          clearTimeout(p.timerId)
          p.reject(
            Exn.raiseError("COMMBUS_NOT_READY: CommBus not initialized"),
          )
        | None => ()
        }
      | Some(bus) =>
        let envelopeJson = Wire.stringifyEnvelope(envelope)
        call(bus, "COMM_BUS_WASM_CALLBACK", relay.config.callEvent, envelopeJson)
        ->Promise.catch(err => {
          switch Map.get(relay.pending, requestId) {
          | Some(p) =>
            Map.delete(relay.pending, requestId)->ignore
            clearTimeout(p.timerId)
            p.reject(err)
          | None => ()
          }
          Promise.resolve(JSON.Encode.null)
        })
        ->ignore
      }
    }
  | _ => () // defensive; caller guarantees Cmd
  }

// Fire-and-forget host event → forward to WASM. No ack expected.
and onHostEvent = (relay: t, event: Wire.wireMsg): unit =>
  switch relay.commBus {
  | None =>
    Console.warn("[msfs-bridge] Cannot forward event — CommBus not ready")
  | Some(bus) =>
    let requestId = nextRequestId(relay)
    let envelope: Wire.commBusRequest = {
      requestId,
      payload: (Obj.magic(event): JSON.t),
    }
    let envelopeJson = Wire.stringifyEnvelope(envelope)
    call(bus, "COMM_BUS_WASM_CALLBACK", relay.config.callEvent, envelopeJson)
    ->Promise.catch(err => {
      Console.error2("[msfs-bridge] Failed to forward event to WASM:", err)
      Promise.resolve(JSON.Encode.null)
    })
    ->ignore
  }

// ---- WASM → relay dispatch ----
//
// Two cases:
//  1. Response envelope {requestId, ok, response?, error?} → resolve or
//     reject the pending promise, which fires the ack to the host.
//  2. A bare WireMsg event/ack → forward verbatim to the host.

let onWasmMessage = (relay: t, raw: string): unit =>
  switch Wire.parseCommBusResponse(raw) {
  | Some(resp) =>
    switch Map.get(relay.pending, resp.requestId) {
    | Some(p) =>
      Map.delete(relay.pending, resp.requestId)->ignore
      clearTimeout(p.timerId)
      if resp.ok {
        p.resolve(resp.response->Option.getOr(JSON.Encode.null))
      } else {
        p.reject(
          Exn.raiseError(
            resp.error->Option.getOr("[msfs-bridge] WASM returned error"),
          ),
        )
      }
    | None => () // orphaned response — pending entry already timed out
    }
  | None =>
    // Not a response envelope — check if it's a bare wire message
    // that we should forward to the host.
    switch Wire.parseWireMsg(raw) {
    | Some(Event(_) | Ack(_)) => wsSend(relay, raw)
    | _ =>
      Console.warn2("[msfs-bridge] Unparseable message from WASM:", raw)
    }
  }

// ---- Public lifecycle ----

let init = (relay: t): unit => {
  connectWs(relay)

  // Capture a stable handler reference for registration/unregistration.
  let handler = raw => onWasmMessage(relay, raw)
  relay.onWasmHandler = Some(handler)

  // Register the CommBus listener. The continuation fires once the
  // listener has been bound by the sim runtime.
  let bus = registerViewListener("JS_LISTENER_COMM_BUS", () => {
    switch (relay.commBus, relay.onWasmHandler) {
    | (Some(b), Some(h)) => on_(b, relay.config.responseEvent, h)
    | _ => ()
    }
  })
  relay.commBus = Some(bus)
}

let destroy = (relay: t): unit => {
  // Reject and clear every pending request so their timers fire no
  // more callbacks.
  Map.forEach(relay.pending, p => {
    clearTimeout(p.timerId)
    p.reject(Exn.raiseError("[msfs-bridge] Relay destroyed"))
  })
  Map.clear(relay.pending)

  // Unhook CommBus. Both off() and unregister() can throw if the sim
  // is tearing down — swallow those.
  switch (relay.commBus, relay.onWasmHandler) {
  | (Some(bus), Some(h)) =>
    try off(bus, relay.config.responseEvent, h) catch {
    | _ => ()
    }
    try unregister(bus) catch {
    | _ => ()
    }
  | _ => ()
  }
  relay.commBus = None
  relay.onWasmHandler = None

  // Cancel any pending reconnect and close the socket.
  switch relay.wsRetryTimerId {
  | Some(id) => clearTimeout(id)
  | None => ()
  }
  relay.wsRetryTimerId = None
  relay.wsRetryCount = 0
  switch relay.ws {
  | Some(ws) =>
    try wsClose(ws) catch {
    | _ => ()
    }
  | None => ()
  }
  relay.ws = None
}
