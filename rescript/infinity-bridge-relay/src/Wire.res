// Wire protocol between host (desktop app) and gauge (in-sim).
//
// The TypeScript original used a discriminated union on a `t` field.
// ReScript's @tag decorator gives us the same runtime shape — a plain
// object with a `t` string — while letting the compiler enforce
// exhaustive pattern matching at every dispatch site.
//
// Keep this file in lockstep with the host-side Rust wire crate.

@tag("t")
type wireMsg =
  | @as("hello")
  Hello({
      client?: string,
      aircraft?: string,
      tail?: string,
      session?: string,
      v?: int,
      meta?: JSON.t,
    })
  | @as("ping") Ping({ts?: float})
  | @as("pong") Pong({ts?: float})
  | @as("cmd")
  Cmd({
      id: string,
      name?: string,
      payload: JSON.t,
    })
  | @as("ack")
  Ack({
      id: string,
      ok: bool,
      error?: string,
      response?: JSON.t,
      duplicate?: bool,
    })
  | @as("event")
  Event({
      name: string,
      data: JSON.t,
    })

// Inner envelope used on the CommBus channel between the relay and the
// WASM gauge. Not part of the host wire protocol — this is what the
// relay uses to correlate WASM responses with host commands.
type commBusRequest = {
  requestId: string,
  payload: JSON.t,
}

type commBusResponse = {
  requestId: string,
  ok: bool,
  response?: JSON.t,
  error?: string,
}

// ---- Parsing ----
//
// Data arrives from the network and from CommBus as JSON strings. We
// validate the shape before casting to our variant types. The @tag
// runtime representation matches the JSON shape exactly, so once we've
// confirmed the discriminator is a known tag, Obj.magic is safe.
//
// These are the ONLY places in the codebase where we use Obj.magic.
// Everywhere else, wireMsg is a real tagged variant with exhaustive
// pattern matching.

let parseWireMsg = (raw: string): option<wireMsg> => {
  switch JSON.parseExn(raw) {
  | json =>
    switch JSON.Classify.classify(json) {
    | Object(dict) =>
      switch Dict.get(dict, "t") {
      | Some(String("hello"))
      | Some(String("ping"))
      | Some(String("pong"))
      | Some(String("cmd"))
      | Some(String("ack"))
      | Some(String("event")) =>
        Some((Obj.magic(json): wireMsg))
      | _ => None
      }
    | _ => None
    }
  | exception _ => None
  }
}

let parseCommBusResponse = (raw: string): option<commBusResponse> => {
  switch JSON.parseExn(raw) {
  | json =>
    switch JSON.Classify.classify(json) {
    | Object(dict) =>
      switch (Dict.get(dict, "requestId"), Dict.get(dict, "ok")) {
      | (Some(String(_)), Some(Boolean(_))) =>
        Some((Obj.magic(json): commBusResponse))
      | _ => None
      }
    | _ => None
    }
  | exception _ => None
  }
}

// ---- Serialization ----
//
// stringifyAny returns None only for values containing functions or
// cycles. Our wire types are plain data so we can safely unwrap with
// "null" as an impossible fallback.

let stringify = (msg: wireMsg): string =>
  JSON.stringifyAny(msg)->Option.getOr("null")

let stringifyEnvelope = (env: commBusRequest): string =>
  JSON.stringifyAny(env)->Option.getOr("null")
