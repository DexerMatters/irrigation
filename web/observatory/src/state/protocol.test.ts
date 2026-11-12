import { describe, expect, it, vi } from "vitest";

/// The store reads `location` to seed its view from the URL and hangs a debugging handle off
/// `window` at import, so it needs a host object before it can be loaded in a node test.
const host: Record<string, unknown> = {};
vi.stubGlobal("location", new URL("http://127.0.0.1:7777/observatory/"));
vi.stubGlobal("window", host);

const store = await import("./store");

/// The handle the store publishes on `window`, which is how the page exposes its state.
const snapshot = () =>
  (host.__observatory as () => { connection: string })();

const hello = (protocol: number) => ({
  protocol,
  level: "nodes",
  runtime: null,
  limits: {},
  history_from: null,
});

describe("the handshake's protocol", () => {
  it("marks the connection live when the page understands the protocol", () => {
    store.applyHello(hello(store.PROTOCOL));
    expect(snapshot().connection).toBe("live");
  });

  /// The server bumps `PROTOCOL` when a field changes meaning, so a reader that ignores it renders a
  /// changed field as if it were the old one -- every symptom a plausible-looking wrong value.
  it("refuses a protocol it was not built against", () => {
    store.applyHello(hello(store.PROTOCOL + 1));
    expect(snapshot().connection).toBe("mismatched");
  });
});
