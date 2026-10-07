import { parseNotes, type Inline } from "../lib/releaseNotes";

function renderInline(inline: Inline[]) {
  return inline.map((part, index) => {
    switch (part.kind) {
      case "code":
        return <code key={index}>{part.text}</code>;
      case "bold":
        return <strong key={index}>{part.text}</strong>;
      case "link":
        return (
          <a key={index} href={part.href} target="_blank" rel="noreferrer noopener">
            {part.text}
          </a>
        );
      default:
        return part.text;
    }
  });
}

/**
 * A release's notes. They are untrusted text from GitHub, so they are read by
 * `parseNotes` into plain values and rendered as elements, never as HTML.
 */
export default function ReleaseNotes({ markdown }: { markdown: string }) {
  return (
    <div className="release-notes">
      {parseNotes(markdown).map((block, index) => {
        switch (block.kind) {
          case "heading":
            return <h4 key={index}>{block.text}</h4>;
          case "list":
            return (
              <ul key={index}>
                {block.items.map((item, itemIndex) => (
                  <li key={itemIndex}>{renderInline(item)}</li>
                ))}
              </ul>
            );
          case "code":
            return (
              <pre key={index}>
                <code>{block.text}</code>
              </pre>
            );
          default:
            return <p key={index}>{renderInline(block.inline)}</p>;
        }
      })}
    </div>
  );
}
