export const WORKLOADS = ["plain", "ansi", "unicode", "wrap", "tui"] as const;
export type Workload = typeof WORKLOADS[number];

export function terminalWorkload(name: Workload, cols: number, rows: number) {
  const marker = "BENCH_DONE";
  let text: string;
  let screen: string[];
  if (name === "tui") {
    const frames: string[] = ["\x1b[?1049h"];
    screen = [];
    for (let frame = 0; frame < 600; frame++) {
      screen = Array.from({ length: rows }, (_, row) => `frame ${frame} row ${row}`);
      frames.push(screen.map((line, row) => `\x1b[${row + 1};1H\x1b[36m${line}\x1b[0m\x1b[K`).join(""));
    }
    screen[rows - 1] = marker;
    frames.push(`\x1b[${rows};1H${marker}\x1b[K`);
    text = frames.join("");
  } else {
    const lines = Array.from({ length: 8000 }, (_, index) => {
      const id = String(index).padStart(5, "0");
      if (name === "unicode") return `${id} 한글 출력 테스트 e\u0301 😀 café`;
      if (name === "wrap") return `${id} ${"0123456789".repeat(Math.ceil(cols / 10) + 2)}`;
      return `${id} firecrab console output ${"0123456789".repeat(3)}`;
    });
    const output = name === "ansi" ? lines.map(line => `\x1b[32m${line}\x1b[0m`) : lines;
    text = `${output.join("\r\n")}\r\n${marker}`;
    const visualLines = name === "wrap"
      ? lines.flatMap(line => Array.from({ length: Math.ceil(line.length / cols) }, (_, part) => line.slice(part * cols, (part + 1) * cols)))
      : lines;
    screen = [...visualLines, marker].slice(-rows);
  }
  return {
    bytes: new TextEncoder().encode(text),
    expected: { text: screen.join("\n"), cursorX: marker.length, cursorY: rows - 1 },
  };
}
