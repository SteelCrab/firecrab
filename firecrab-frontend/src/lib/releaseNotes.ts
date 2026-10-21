/**
 * A small, safe reading of a release's Markdown notes.
 *
 * The notes come from GitHub, so they are untrusted text. They are never
 * turned into HTML: this produces a tree of plain values that React renders
 * as elements, with links kept only when they are `https://`.
 */

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "bold"; text: string }
  | { kind: "link"; text: string; href: string };

export type Block =
  | { kind: "heading"; text: string }
  | { kind: "paragraph"; inline: Inline[] }
  | { kind: "list"; items: Inline[][] }
  | { kind: "code"; text: string };

/** `[#123]: https://…`, the reference definitions a changelog ends with. */
const DEFINITION = /^\[([^\]]+)\]:\s*(\S+)\s*$/;
const SAFE_URL = /^https:\/\/[^\s<>"']+$/;
/** Code, bold, `[text](url)`, and a bare `[reference]`, each matched in one pass. */
const INLINE = /`([^`]+)`|\*\*([^*]+)\*\*|\[([^\]]+)\]\(([^)\s]+)\)|\[([^\]]+)\](?!\()/g;

export function parseInline(text: string, references: Map<string, string>): Inline[] {
  const out: Inline[] = [];
  let last = 0;
  for (const match of text.matchAll(INLINE)) {
    const index = match.index ?? 0;
    if (index > last) out.push({ kind: "text", text: text.slice(last, index) });
    if (match[1] !== undefined) {
      out.push({ kind: "code", text: match[1] });
    } else if (match[2] !== undefined) {
      out.push({ kind: "bold", text: match[2] });
    } else if (match[3] !== undefined) {
      out.push(SAFE_URL.test(match[4]) ? { kind: "link", text: match[3], href: match[4] } : { kind: "text", text: match[3] });
    } else {
      // `[#123]` reads as the issue number it is; a bracket that names nothing stays as written.
      const href = references.get(match[5]);
      out.push(href ? { kind: "link", text: match[5], href } : { kind: "text", text: `[${match[5]}]` });
    }
    last = index + match[0].length;
  }
  if (last < text.length) out.push({ kind: "text", text: text.slice(last) });
  return out;
}

export function parseNotes(markdown: string): Block[] {
  const references = new Map<string, string>();
  const lines: string[] = [];
  for (const line of markdown.replace(/\r\n?/g, "\n").split("\n")) {
    const definition = DEFINITION.exec(line);
    if (!definition) {
      lines.push(line);
    } else if (SAFE_URL.test(definition[2])) {
      references.set(definition[1], definition[2]);
    }
  }

  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let list: string[][] | null = null;
  let code: string[] | null = null;
  const endParagraph = () => {
    if (paragraph.length > 0) blocks.push({ kind: "paragraph", inline: parseInline(paragraph.join(" "), references) });
    paragraph = [];
  };
  const endList = () => {
    if (list) blocks.push({ kind: "list", items: list.map((item) => parseInline(item.join(" "), references)) });
    list = null;
  };

  for (const raw of lines) {
    if (code !== null) {
      if (raw.trimStart().startsWith("```")) {
        blocks.push({ kind: "code", text: code.join("\n") });
        code = null;
      } else {
        code.push(raw);
      }
      continue;
    }
    const line = raw.trimEnd();
    if (line.trimStart().startsWith("```")) {
      endParagraph();
      endList();
      code = [];
      continue;
    }
    if (line.trim() === "") {
      endParagraph();
      endList();
      continue;
    }
    const heading = /^#{1,6}\s+(.*)$/.exec(line);
    if (heading) {
      endParagraph();
      endList();
      blocks.push({ kind: "heading", text: heading[1].trim() });
      continue;
    }
    const item = /^[-*]\s+(.*)$/.exec(line);
    if (item) {
      endParagraph();
      list ??= [];
      list.push([item[1]]);
      continue;
    }
    if (list) {
      // A wrapped line belongs to the item above it.
      list[list.length - 1].push(line.trim());
      continue;
    }
    paragraph.push(line.trim());
  }
  if (code !== null) blocks.push({ kind: "code", text: code.join("\n") });
  endParagraph();
  endList();
  return blocks;
}
