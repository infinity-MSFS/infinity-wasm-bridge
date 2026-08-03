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
//  2. WebSocket connection lifecycle with fixed-interval reconnect.

open MsfsBindings

exception BridgeError(string)

// Gauge → host connection-state event. Announces that the CommBus listener
// is bound, i.e. that a command sent to this relay can actually reach a WASM
// module. Must match `infinity_bridge_wire::READY_EVENT`.
let readyEventName = "__bridge_ready"

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
  /** Fixed reconnect interval in ms. Defaults to 1000. */
  reconnectMs?: int,
  protocolVersion?: int,
  /**
   * How long to wait for the WASM module to answer before acking the host
   * with `WASM_TIMEOUT`. Defaults to 2500.
   *
   * **Keep this below the host's own command timeout.** If the host gives up
   * first, its caller sees a generic transport timeout and this relay's
   * diagnosis of *why* nothing answered — module absent, CommBus unbound,
   * handler threw — is discarded with the request.
   */
  requestTimeoutMs?: int,
  /** Retry interval for a CommBus bind that hasn't taken. Defaults to 1000. */
  bindRetryMs?: int,
}

type resolvedConfig = {
  wsUrl: string,
  callEvent: string,
  responseEvent: string,
  hello: helloConfig,
  dedupCapacity: int,
  reconnectMs: int,
  protocolVersion: int,
  requestTimeoutMs: int,
  bindRetryMs: int,
}

// ---- Relay state ----

type t = {
  config: resolvedConfig,
  mutable commBus: option<viewListener>,
  // Whether the response listener is actually attached to the CommBus. The
  // handle existing is not the same thing — see `init`.
  mutable commBusBound: bool,
  mutable bindRetryTimerId: option<timerId>,
  mutable ws: option<webSocket>,
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
  reconnectMs: c.reconnectMs->Option.getOr(1_000),
  protocolVersion: c.protocolVersion->Option.getOr(1),
  requestTimeoutMs: c.requestTimeoutMs->Option.getOr(2500),
  bindRetryMs: c.bindRetryMs->Option.getOr(1_000),
}

let make = (config: config): t => {
  let resolved = resolveConfig(config)
  {
    config: resolved,
    commBus: None,
    commBusBound: false,
    bindRetryTimerId: None,
    ws: None,
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

// ---- Readiness ----
//
// Tell the host whether this relay can currently reach a WASM module. An
// open socket only proves the Coherent gauge is alive: the panel loads long
// before the aircraft's systems do, and a relay whose CommBus never bound
// looks identical from the far end. Sent on every edge that can change the
// answer (bind, WS open), so a host that missed one gets the next.

let sendReady = (relay: t): unit => {
  let evt: Wire.wireMsg = Event({
    name: readyEventName,
    data: JSON.Encode.object(
      Dict.fromArray([("ready", JSON.Encode.bool(relay.commBusBound))]),
    ),
  })
  wsSend(relay, Wire.stringify(evt))
}

// Attach the response listener. Idempotent: `init` binds from two paths
// because either one can be the one that wins the race, and a retry timer
// covers the case where both lose.
let bindCommBus = (relay: t, bus: viewListener): unit =>
  if !relay.commBusBound {
    switch relay.onWasmHandler {
    | Some(h) =>
      try {
        on_(bus, relay.config.responseEvent, h)
        relay.commBusBound = true
        sendReady(relay)
      } catch {
      | exn => Console.error2("[msfs-bridge] CommBus bind failed:", exn)
      }
    | None => ()
    }
  }

let rec scheduleBindRetry = (relay: t): unit =>
  if !relay.commBusBound {
    switch relay.bindRetryTimerId {
    | Some(_) => ()
    | None =>
      let timer = setTimeout(() => {
        relay.bindRetryTimerId = None
        switch relay.commBus {
        | Some(bus) => bindCommBus(relay, bus)
        | None => ()
        }
        scheduleBindRetry(relay)
      }, relay.config.bindRetryMs)
      relay.bindRetryTimerId = Some(timer)
    }
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
        // A reconnect lands on a fresh client record on the host, which
        // starts out not-ready however long we've been bound down here.
        sendReady(relay)
      })

      setOnMessage(ws, ev => onHostMessage(relay, ev.data))
      // Coherent (MSFS) often fires `onerror` WITHOUT a following `onclose` on a
      // failed/dropped connect, so reschedule here too — otherwise the relay is
      // left stuck with a dead socket and never retries. Guarded by the retry
      // timer, so a following `onclose` is harmless.
      setOnError(ws, () => {
        relay.ws = None
        scheduleWsReconnect(relay)
      })
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
    let timer = setTimeout(() => {
      relay.wsRetryTimerId = None
      connectWs(relay)
    }, relay.config.reconnectMs)
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
        // Distinguish "nobody answered" from "nobody could have answered" —
        // the two have completely different fixes, and the host can only
        // report what we put in the ack.
        sendAck(
          relay,
          ~id=cmdId,
          ~ok=false,
          ~error=relay.commBusBound
            ? "WASM_TIMEOUT: no response from WASM gauge"
            : "COMMBUS_NOT_BOUND: response listener never attached — no WASM module is reachable",
        )
      }, relay.config.requestTimeoutMs)

      Map.set(
        relay.pending,
        requestId,
        {
          resolve: response => sendAck(relay, ~id=cmdId, ~ok=true, ~response),
          reject: err => {
            let msg = switch err {
            | BridgeError(m) => m
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
            BridgeError("COMMBUS_NOT_READY: CommBus not initialized"),
          )
        | None => ()
        }
      | Some(bus) =>
        // Recover a bind that never took before spending a request on it.
        bindCommBus(relay, bus)
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
          BridgeError(
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

  // Register the CommBus listener.
  //
  // The continuation fires once the sim runtime has bound the listener —
  // which may be LATER, or may be RIGHT NOW. It fires synchronously whenever
  // the view listener is already connected: a second relay in the same gauge,
  // or any panel reload after the first. Reading `relay.commBus` from inside
  // it loses that case, because the assignment below hasn't run yet, and the
  // silent `_ => ()` fallthrough then leaves the response listener unbound
  // for the rest of the session. Every request the host makes times out, the
  // socket stays open the whole time, and nothing anywhere reports a fault.
  //
  // So bind through a ref both paths can see, bind again unconditionally once
  // the handle is in hand, and keep retrying if neither took. `bindCommBus`
  // is idempotent, so exactly one listener is registered whichever path wins.
  let busRef = ref(None)
  let bus = registerViewListener("JS_LISTENER_COMM_BUS", () =>
    switch busRef.contents {
    | Some(b) => bindCommBus(relay, b)
    | None => () // fired synchronously — the bind below covers it
    }
  )
  busRef := Some(bus)
  relay.commBus = Some(bus)
  bindCommBus(relay, bus)
  scheduleBindRetry(relay)
}

let destroy = (relay: t): unit => {
  // Reject and clear every pending request so their timers fire no
  // more callbacks.
  Map.forEach(relay.pending, p => {
    clearTimeout(p.timerId)
    p.reject(BridgeError("[msfs-bridge] Relay destroyed"))
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
  relay.commBusBound = false
  relay.onWasmHandler = None
  switch relay.bindRetryTimerId {
  | Some(id) => clearTimeout(id)
  | None => ()
  }
  relay.bindRetryTimerId = None

  // Cancel any pending reconnect and close the socket.
  switch relay.wsRetryTimerId {
  | Some(id) => clearTimeout(id)
  | None => ()
  }
  relay.wsRetryTimerId = None
  switch relay.ws {
  | Some(ws) =>
    try wsClose(ws) catch {
    | _ => ()
    }
  | None => ()
  }
  relay.ws = None
}
