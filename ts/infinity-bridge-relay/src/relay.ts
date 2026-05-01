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
	maxReconnectMs?: number;
	baseReconnectMs?: number;
	protocolVersion?: number;
}

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
			| "maxReconnectMs"
			| "baseReconnectMs"
			| "protocolVersion"
		>
	> & { hello: NonNullable<BridgeRelayConfig["hello"]> };

	private commBus?: ViewListener.ViewListener;
	private ws?: WebSocket;
	private wsRetryCount = 0;
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
			maxReconnectMs: config.maxReconnectMs ?? 30_000,
			baseReconnectMs: config.baseReconnectMs ?? 250,
			protocolVersion: config.protocolVersion ?? 1,
		};
		this.dedup = new DedupRing(this.config.dedupCapacity);
	}

	init(): void {
		this.connectWs();

		this.commBus = RegisterViewListener("JS_LISTENER_COMM_BUS", () => {
			this.commBus!.on(this.config.responseEvent, this.onWasmMessage);
		});
	}

	destroy(): void {
		for (const [, p] of this.pending) {
			clearTimeout(p.timerId);
			p.reject(new Error("[msfs-bridge] Relay destroyed"));
		}
		this.pending.clear();

		try {
			this.commBus?.off?.(this.config.responseEvent, this.onWasmMessage);
		} catch {
			/* ignore */
		}
		this.commBus?.unregister?.();
		this.commBus = undefined;

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
				this.wsRetryCount = 0;
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
			};

			this.ws.onmessage = (ev: MessageEvent) => {
				this.onHostMessage(ev.data);
			};

			this.ws.onerror = () => {};

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
		const delay = this.backoff(this.wsRetryCount++);
		console.log(
			`[msfs-bridge] Reconnecting in ${delay}ms (attempt ${this.wsRetryCount})`,
		);
		this.wsRetryTimerId = window.setTimeout(() => {
			this.wsRetryTimerId = undefined;
			this.connectWs();
		}, delay);
	}

	private backoff(attempt: number): number {
		const base = Math.min(
			this.config.maxReconnectMs,
			this.config.baseReconnectMs * Math.pow(2, Math.min(attempt, 10)),
		);
		return base + Math.floor(Math.random() * 250);
	}

	private cleanupWs(): void {
		if (this.wsRetryTimerId !== undefined) {
			clearTimeout(this.wsRetryTimerId);
			this.wsRetryTimerId = undefined;
		}
		this.wsRetryCount = 0;
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

		const timeoutMs = 5000;

		const timerId = window.setTimeout(() => {
			this.pending.delete(requestId);
			this.wsSend(
				JSON.stringify({
					t: "ack",
					id: cmdId,
					ok: false,
					error: "WASM_TIMEOUT: no response from WASM gauge",
				} satisfies AckMsg),
			);
		}, timeoutMs);

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

type PongMsg = import("./wire").PongMsg;
type CmdMsg = import("./wire").CmdMsg;
type AckMsg = import("./wire").AckMsg;
type EventMsg = import("./wire").EventMsg;
type CommBusRequest = import("./wire").CommBusRequest;
