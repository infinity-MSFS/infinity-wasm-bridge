export type WireMsg = HelloMsg | PingMsg | PongMsg | CmdMsg | AckMsg | EventMsg;

export interface HelloMsg {
	t: "hello";
	client?: string;
	aircraft?: string;
	tail?: string;
	session?: string;
	v?: number;
	meta?: unknown;
}

export interface PingMsg {
	t: "ping";
	ts?: number;
}

export interface PongMsg {
	t: "pong";
	ts?: number;
}

export interface CmdMsg {
	t: "cmd";
	id: string;
	name?: string;
	payload: unknown;
}

export interface AckMsg {
	t: "ack";
	id: string;
	ok: boolean;
	error?: string;
	response?: unknown;
	duplicate?: boolean;
}

export interface EventMsg {
	t: "event";
	name: string;
	data: unknown;
}

export interface CommBusRequest {
	requestId: string;
	payload: unknown;
}

export interface CommBusResponse {
	requestId: string;
	ok: boolean;
	response?: unknown;
	error?: string;
}
