/// <reference types="@microsoft/msfs-types/pages/vcockpit/core/vcockpit" />
/// <reference types="@microsoft/msfs-types/js/common" />

import { BridgeRelay } from "@infinity-msfs/ts-bridge-relay";

class MyAddonBridgeInstrument extends BaseInstrument {
	private relay?: BridgeRelay;

	get templateID(): string {
		return "MyAddon_Bridge";
	}

	// ── Lifecycle ──────────────────────────────────────────────────

	public connectedCallback(): void {
		super.connectedCallback();

		this.relay = new BridgeRelay({
			// ── Connection ─────────────────────────────────────────────
			// The WebSocket URL of your host application.
			// Must match what your host's BridgeServer is listening on.
			wsUrl: "ws://127.0.0.1:6969/bridge",

			// ── CommBus event names ────────────────────────────────────
			// These must match the BridgeConfig in your WASM gauge.
			// Convention: "youraddon/bridge_call" and "youraddon/bridge_resp"
			callEvent: "myaddon/bridge_call",
			responseEvent: "myaddon/bridge_resp",

			// ── Hello payload ──────────────────────────────────────────
			// Sent to the host on each WebSocket connect. The host can
			// use this to identify which aircraft/session is connected.
			hello: {
				client: "msfs-gauge",
				aircraft: "DC-10-30",
				// session is auto-generated from Date.now() if not provided
			},

			// ── Optional tuning ────────────────────────────────────────
			// dedupCapacity: 128,      // default: 128
			// maxReconnectMs: 30_000,  // default: 30s
			// baseReconnectMs: 250,    // default: 250ms
			// protocolVersion: 1,      // default: 1
		});

		this.relay.init();
		console.log("[MyAddon] Bridge relay initialized");
	}

	public disconnectedCallback(): void {
		this.relay?.destroy();
		this.relay = undefined;
		console.log("[MyAddon] Bridge relay destroyed");

		super.disconnectedCallback();
	}
}

registerInstrument("myaddon-bridge-instrument", MyAddonBridgeInstrument);
