/// <reference types="@microsoft/msfs-types/pages/vcockpit/core/vcockpit" />
/// <reference types="@microsoft/msfs-types/js/common" />

import type {
	WireMsg,
	CmdMsg,
	AckMsg,
	PongMsg,
	EventMsg,
	CommBusRequest,
	CommBusResponse,
} from "./wire";
import { DedupRing } from "./dedup";

export interface BridgeRelayConfig {
	wsUrl: string;
	callEvent: string;
	responseEvent: string;
	hello?: {
		client?: string;
		aircraft?: string;
		tail?: string;
		session?: string;
		meta?: unknown;
	};
	dedupCapacity?: number;
	/** Fixed reconnect interval in ms. Defaults to 1000. */
	reconnectMs?: number;
	protocolVersion?: number;
	/**
	 * How long to wait for the WASM module to answer before acking the host
	 * with `WASM_TIMEOUT`. Defaults to 2500.
	 *
	 * **Keep this below the host's own command timeout.** If the host gives up
	 * first, its caller sees a generic transport timeout and this relay's
	 * diagnosis of *why* nothing answered is discarded with the request.
	 */
	requestTimeoutMs?: number;
	/** Retry interval for a CommBus bind that hasn't taken. Defaults to 1000. */
	bindRetryMs?: number;
}

/**
 * Gauge → host connection-state event. Announces that the CommBus listener is
 * bound, i.e. that a command sent to this relay can actually reach a WASM
 * module. Must match `infinity_bridge_wire::READY_EVENT`.
 */
const READY_EVENT = "__bridge_ready";

interface PendingRequest<T = unknown> {
	resolve: (v: T) => void;
	reject: (e: Error) => void;
	timerId: number;
}

export class BridgeRelay {
	private readonly config: Required<
		Pick<
			BridgeRelayConfig,
			| "wsUrl"
			| "callEvent"
			| "responseEvent"
			| "dedupCapacity"
			| "reconnectMs"
			| "protocolVersion"
			| "requestTimeoutMs"
			| "bindRetryMs"
		>
	> & { hello: NonNullable<BridgeRelayConfig["hello"]> };

	private commBus?: ViewListener.ViewListener;
	/**
	 * Whether the response listener is actually attached to the CommBus. The
	 * handle existing is not the same thing — see {@link init}.
	 */
	private commBusBound = false;
	private bindRetryTimerId?: number;
	private ws?: WebSocket;
	private wsRetryTimerId?: number;

	private readonly pending = new Map<string, PendingRequest>();
	private requestSeq = 0;
	private readonly dedup: DedupRing;

	constructor(config: BridgeRelayConfig) {
		this.config = {
			wsUrl: config.wsUrl,
			callEvent: config.callEvent,
			responseEvent: config.responseEvent,
			hello: config.hello ?? { client: "msfs-gauge" },
			dedupCapacity: config.dedupCapacity ?? 128,
			reconnectMs: config.reconnectMs ?? 1_000,
			protocolVersion: config.protocolVersion ?? 1,
			requestTimeoutMs: config.requestTimeoutMs ?? 2_500,
			bindRetryMs: config.bindRetryMs ?? 1_000,
		};
		this.dedup = new DedupRing(this.config.dedupCapacity);
	}

	init(): void {
		this.connectWs();

		// The continuation fires once the sim runtime has bound the listener —
		// which may be LATER, or may be RIGHT NOW. It fires synchronously
		// whenever the view listener is already connected: a second relay in the
		// same gauge, or any panel reload after the first. Reading `this.commBus`
		// from inside it loses that case, because the assignment below hasn't run
		// yet — the non-null assertion throws, the response listener never binds,
		// and every request the host makes times out for the rest of the session
		// while the socket stays open the whole time.
		//
		// So bind through a local both paths can see, bind again unconditionally
		// once the handle is in hand, and keep retrying if neither took.
		// `bindCommBus` is idempotent, so exactly one listener is registered.
		let bus: ViewListener.ViewListener | undefined;
		const handle = RegisterViewListener("JS_LISTENER_COMM_BUS", () => {
			// Undefined here means we fired synchronously; the bind below covers it.
			if (bus) this.bindCommBus(bus);
		});
		bus = handle;
		this.commBus = handle;
		this.bindCommBus(handle);
		this.scheduleBindRetry();
	}

	/**
	 * Attach the response listener. Idempotent: {@link init} binds from two
	 * paths because either one can be the one that wins the race, and a retry
	 * timer covers the case where both lose.
	 */
	private bindCommBus(bus: ViewListener.ViewListener): void {
		if (this.commBusBound) return;
		try {
			bus.on(this.config.responseEvent, this.onWasmMessage);
			this.commBusBound = true;
			this.sendReady();
		} catch (e) {
			console.error("[msfs-bridge] CommBus bind failed:", e);
		}
	}

	private scheduleBindRetry(): void {
		if (this.commBusBound) return;
		if (this.bindRetryTimerId !== undefined) return;
		this.bindRetryTimerId = window.setTimeout(() => {
			this.bindRetryTimerId = undefined;
			if (this.commBus) this.bindCommBus(this.commBus);
			this.scheduleBindRetry();
		}, this.config.bindRetryMs);
	}

	/**
	 * Tell the host whether this relay can currently reach a WASM module. An
	 * open socket only proves the Coherent gauge is alive: the panel loads long
	 * before the aircraft's systems do, and a relay whose CommBus never bound
	 * looks identical from the far end. Sent on every edge that can change the
	 * answer (bind, WS open), so a host that missed one gets the next.
	 */
	private sendReady(): void {
		this.wsSend(
			JSON.stringify({
				t: "event",
				name: READY_EVENT,
				data: { ready: this.commBusBound },
			} satisfies EventMsg),
		);
	}

	destroy(): void {
		for (const [, p] of this.pending) {
			clearTimeout(p.timerId);
			p.reject(new Error("[msfs-bridge] Relay destroyed"));
		}
		this.pending.clear();

		if (this.bindRetryTimerId !== undefined) {
			clearTimeout(this.bindRetryTimerId);
			this.bindRetryTimerId = undefined;
		}

		try {
			this.commBus?.off?.(this.config.responseEvent, this.onWasmMessage);
		} catch {
			/* ignore */
		}
		this.commBus?.unregister?.();
		this.commBus = undefined;
		this.commBusBound = false;

		this.cleanupWs();
	}

	/**
	 * Called when WASM sends a message on the response CommBus event.
	 *
	 * Two cases:
	 * 1. Response to a pending request (has `requestId`) → resolve promise,
	 *    then forward as ack to WebSocket.
	 * 2. Fire-and-forget event (WireMsg with `t: "event"`) → forward to
	 *    WebSocket directly.
	 */
	private onWasmMessage = (raw: string): void => {
		let parsed: Record<string, unknown>;
		try {
			parsed = JSON.parse(typeof raw === "string" ? raw : "");
		} catch {
			console.warn("[msfs-bridge] Failed to parse WASM message:", raw);
			return;
		}

		if (typeof parsed.requestId === "string") {
			const resp = parsed as unknown as CommBusResponse;
			const pending = this.pending.get(resp.requestId);
			if (pending) {
				this.pending.delete(resp.requestId);
				clearTimeout(pending.timerId);

				if (resp.ok === false) {
					pending.reject(
						new Error(resp.error ?? "[msfs-bridge] WASM returned error"),
					);
				} else {
					pending.resolve(resp.response);
				}
			}

			// If this was a cmd response, also forward ack to host
			// We need the original cmd id — stored in our pending map keyed by requestId.
			// But we've already consumed it. The host correlates by its own cmd.id,
			// which was included in the payload sent to WASM. The WASM bridge responds
			// with just requestId. We need a mapping.
			//
			// Actually, let's reconsider the flow. When the host sends cmd {id: "abc"},
			// the relay wraps it in a CommBus envelope {requestId: "relay-123", payload: <the cmd wire msg>}.
			// WASM responds with {requestId: "relay-123", ok, response}.
			// The relay needs to unwrap this back into an ack {t: "ack", id: "abc", ok, response}.
			//
			// So we store the original cmd.id alongside the pending request.
			// This is handled in onHostCommand — see the `cmdId` field.
			return;
		}

		if (parsed.t === "event" || parsed.t === "ack") {
			this.wsSend(raw);
		}
	};

	private connectWs(): void {
		if (
			this.ws &&
			(this.ws.readyState === WebSocket.OPEN ||
				this.ws.readyState === WebSocket.CONNECTING)
		) {
			return;
		}

		try {
			this.ws = new WebSocket(this.config.wsUrl);

			this.ws.onopen = () => {
				console.log("[msfs-bridge] WebSocket connected");
				this.wsSend(
					JSON.stringify({
						t: "hello",
						client: this.config.hello.client ?? "msfs-gauge",
						aircraft: this.config.hello.aircraft,
						tail: this.config.hello.tail,
						session: this.config.hello.session ?? String(Date.now()),
						v: this.config.protocolVersion,
						meta: this.config.hello.meta,
					}),
				);
				// A reconnect lands on a fresh client record on the host, which
				// starts out not-ready however long we've been bound down here.
				this.sendReady();
			};

			this.ws.onmessage = (ev: MessageEvent) => {
				this.onHostMessage(ev.data);
			};

			// Coherent (MSFS) often fires `onerror` WITHOUT a following `onclose`
			// on a failed/dropped connect, so reschedule here too — otherwise the
			// relay is left holding a dead socket and never retries. Guarded by
			// the retry timer, so a following `onclose` is harmless.
			this.ws.onerror = () => {
				this.ws = undefined;
				this.scheduleWsReconnect();
			};

			this.ws.onclose = () => {
				this.ws = undefined;
				this.scheduleWsReconnect();
			};
		} catch {
			this.ws = undefined;
			this.scheduleWsReconnect();
		}
	}

	private scheduleWsReconnect(): void {
		if (this.wsRetryTimerId !== undefined) return;
		this.wsRetryTimerId = window.setTimeout(() => {
			this.wsRetryTimerId = undefined;
			this.connectWs();
		}, this.config.reconnectMs);
	}

	private cleanupWs(): void {
		if (this.wsRetryTimerId !== undefined) {
			clearTimeout(this.wsRetryTimerId);
			this.wsRetryTimerId = undefined;
		}
		try {
			this.ws?.close();
		} catch {
			/* ignore */
		}
		this.ws = undefined;
	}

	private wsSend(text: string): void {
		try {
			if (this.ws?.readyState === WebSocket.OPEN) {
				this.ws.send(text);
			}
		} catch (e) {
			console.error("[msfs-bridge] WebSocket send failed:", e);
		}
	}

	private onHostMessage(data: unknown): void {
		if (typeof data !== "string") return;

		let msg: Record<string, unknown>;
		try {
			msg = JSON.parse(data);
		} catch {
			console.warn("[msfs-bridge] Non-JSON from host:", data);
			return;
		}

		const t = msg.t as string;

		switch (t) {
			case "ping":
				this.wsSend(
					JSON.stringify({ t: "pong", ts: Date.now() } satisfies PongMsg),
				);
				break;

			case "cmd":
				this.onHostCommand(msg as unknown as CmdMsg);
				break;

			case "event":
				this.onHostEvent(msg as unknown as EventMsg);
				break;

			default:
				// Unknown message type — ignore
				break;
		}
	}

	/**
	 * Handle a command from the host.
	 *
	 * Wraps the command in a CommBus envelope, sends to WASM, and when
	 * the WASM response arrives, unwraps it back into a wire ack and
	 * sends to the host.
	 */
	private onHostCommand(cmd: CmdMsg): void {
		const cmdId = cmd.id;
		if (typeof cmdId !== "string") return;

		if (this.dedup.has(cmdId)) {
			this.wsSend(
				JSON.stringify({
					t: "ack",
					id: cmdId,
					ok: true,
					response: null,
					duplicate: true,
				} satisfies AckMsg),
			);
			return;
		}
		this.dedup.mark(cmdId);

		const requestId = this.nextRequestId();
		const envelope: CommBusRequest = {
			requestId,
			payload: cmd,
		};

		const timerId = window.setTimeout(() => {
			this.pending.delete(requestId);
			// Distinguish "nobody answered" from "nobody could have answered" —
			// the two have completely different fixes, and the host can only
			// report what we put in the ack.
			this.wsSend(
				JSON.stringify({
					t: "ack",
					id: cmdId,
					ok: false,
					error: this.commBusBound
						? "WASM_TIMEOUT: no response from WASM gauge"
						: "COMMBUS_NOT_BOUND: response listener never attached — no WASM module is reachable",
				} satisfies AckMsg),
			);
		}, this.config.requestTimeoutMs);

		this.pending.set(requestId, {
			resolve: (response: unknown) => {
				this.wsSend(
					JSON.stringify({
						t: "ack",
						id: cmdId,
						ok: true,
						response,
					} satisfies AckMsg),
				);
			},
			reject: (err: Error) => {
				this.wsSend(
					JSON.stringify({
						t: "ack",
						id: cmdId,
						ok: false,
						error: err.message,
					} satisfies AckMsg),
				);
			},
			timerId,
		});

		if (!this.commBus) {
			const p = this.pending.get(requestId);
			if (p) {
				this.pending.delete(requestId);
				clearTimeout(p.timerId);
				p.reject(new Error("COMMBUS_NOT_READY: CommBus not initialized"));
			}
			return;
		}

		// Recover a bind that never took before spending a request on it.
		this.bindCommBus(this.commBus);

		this.commBus
			.call(
				"COMM_BUS_WASM_CALLBACK",
				this.config.callEvent,
				JSON.stringify(envelope),
			)
			.catch((err: unknown) => {
				const p = this.pending.get(requestId);
				if (!p) return;
				this.pending.delete(requestId);
				clearTimeout(p.timerId);
				p.reject(err instanceof Error ? err : new Error(String(err)));
			});
	}

	/**
	 * Handle a fire-and-forget event from the host.
	 *
	 * Wraps in a CommBus envelope and sends to WASM. No ack expected.
	 */
	private onHostEvent(event: EventMsg): void {
		if (!this.commBus) {
			console.warn("[msfs-bridge] Cannot forward event — CommBus not ready");
			return;
		}

		const requestId = this.nextRequestId();
		const envelope: CommBusRequest = {
			requestId,
			payload: event,
		};

		this.commBus
			.call(
				"COMM_BUS_WASM_CALLBACK",
				this.config.callEvent,
				JSON.stringify(envelope),
			)
			.catch((err: unknown) => {
				console.error("[msfs-bridge] Failed to forward event to WASM:", err);
			});
	}

	private nextRequestId(): string {
		return `${Date.now()}-${++this.requestSeq}`;
	}
}
