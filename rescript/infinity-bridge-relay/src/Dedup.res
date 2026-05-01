// Bounded LRU ring for deduplicating command IDs.
//
// The host may retry commands under network stress. When a retry arrives
// with a previously-seen ID, we reply with an ack that has duplicate=true
// without re-dispatching to WASM.

type t = {
  set: Set.t<string>,
  order: array<string>,
  capacity: int,
}

let make = (~capacity: int=128): t => {
  set: Set.make(),
  order: [],
  capacity,
}

let has = (ring: t, id: string): bool => Set.has(ring.set, id)

let mark = (ring: t, id: string): unit =>
  if !Set.has(ring.set, id) {
    Set.add(ring.set, id)
    Array.push(ring.order, id)

    // Evict oldest entries until we're back under capacity. Using
    // Array.shift is O(n) but capacity is small (128 default) and this
    // only runs on fresh IDs, so it's fine in practice.
    while Array.length(ring.order) > ring.capacity {
      switch Array.shift(ring.order) {
      | Some(old) => Set.delete(ring.set, old)->ignore
      | None => ()
      }
    }
  }

let clear = (ring: t): unit => {
  Set.clear(ring.set)
  while Array.length(ring.order) > 0 {
    Array.pop(ring.order)->ignore
  }
}
