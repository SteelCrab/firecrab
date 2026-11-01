import { createHash } from "node:crypto";

// A real negotiated WebSocket: Playwright's route mock does not model subprotocols.
export function consoleResizeFixture(fixture) {
  const viewers = new Map();
  const received = [];
  let available = true;
  let closing = false;
  const frame = (socket, value, opcode = 1) => {
    const bytes = Buffer.from(value);
    if (bytes.length >= 126)
      throw Error("Console fixture frames must remain small");
    if (!socket.destroyed && socket.writable)
      socket.write(
        Buffer.concat([Buffer.from([0x80 | opcode, bytes.length]), bytes]),
      );
  };
  const broadcast = () => {
    if (closing) return;
    const sizes = [...viewers.values()].filter(Boolean);
    const size = sizes.length
      ? {
          rows: Math.min(...sizes.map((s) => s.rows)),
          cols: Math.min(...sizes.map((s) => s.cols)),
        }
      : null;
    for (const socket of viewers.keys())
      frame(
        socket,
        JSON.stringify({
          type: "resize",
          status: available ? "applied" : "unavailable",
          size,
          viewers: viewers.size,
        }),
      );
  };
  fixture.onUpgrade((request, socket) => {
    const requested = request.headers["sec-websocket-protocol"]?.split(/,\s*/);
    if (!requested?.includes("firecrab.console.v1")) {
      socket.destroy();
      return;
    }
    const accept = createHash("sha1")
      .update(
        `${request.headers["sec-websocket-key"]}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`,
      )
      .digest("base64");
    socket.write(
      `HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\nSec-WebSocket-Protocol: firecrab.console.v1\r\n\r\n`,
    );
    viewers.set(socket, null);
    frame(
      socket,
      JSON.stringify({
        type: "resize",
        status: "pending",
        size: null,
        viewers: viewers.size,
      }),
    );
    frame(socket, "guest resize ready\r\nprompt# ", 2);
    let pending = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      pending = Buffer.concat([pending, chunk]);
      while (pending.length >= 2) {
        const opcode = pending[0] & 15;
        let size = pending[1] & 127,
          offset = 2;
        if (size === 126) {
          if (pending.length < 4) return;
          size = pending.readUInt16BE(2);
          offset = 4;
        } else if (size === 127) {
          socket.destroy();
          return;
        }
        if (!(pending[1] & 128)) {
          socket.destroy();
          return;
        }
        if (pending.length < offset + 4 + size) return;
        const mask = pending.subarray(offset, offset + 4);
        const bytes = Buffer.from(
          pending.subarray(offset + 4, offset + 4 + size),
        );
        for (let i = 0; i < bytes.length; i++) bytes[i] ^= mask[i % 4];
        pending = pending.subarray(offset + 4 + size);
        if (opcode === 8) {
          frame(socket, "", 8);
          socket.end();
          return;
        }
        if (opcode === 1) {
          const value = JSON.parse(bytes.toString());
          received.push(value);
          if (value.type === "resize") {
            viewers.set(socket, { rows: value.rows, cols: value.cols });
            broadcast();
          }
        } else if (opcode === 2) received.push(bytes.toString());
      }
    });
    socket.on("error", () => {});
    socket.on("close", () => {
      viewers.delete(socket);
      broadcast();
    });
  });
  return {
    received,
    unavailable() {
      available = false;
      broadcast();
    },
    close() {
      closing = true;
      for (const socket of viewers.keys()) socket.destroy();
    },
  };
}
