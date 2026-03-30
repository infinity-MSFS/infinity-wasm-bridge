export class DedupRing {
	private readonly set = new Set<string>();
	private readonly order: string[] = [];
	private readonly capacity: number;

	constructor(capacity = 128) {
		this.capacity = capacity;
	}

	has(id: string): boolean {
		return this.set.has(id);
	}

	mark(id: string): void {
		if (this.set.has(id)) return;

		this.set.add(id);
		this.order.push(id);

		while (this.order.length > this.capacity) {
			const old = this.order.shift();
			if (old !== undefined) this.set.delete(old);
		}
	}

	clear(): void {
		this.set.clear();
		this.order.length = 0;
	}
}
