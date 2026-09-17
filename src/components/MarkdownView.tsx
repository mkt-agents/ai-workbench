import { useMemo, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy } from "lucide-react";

/**
 * Lightweight dependency-free Markdown renderer for quick-ask answers.
 * Builds React elements only (no innerHTML) so streamed/untrusted model
 * output cannot inject markup. Supports: fenced code blocks (with language
 * label + copy), headings, ordered/unordered lists, quotes, hr, inline
 * code/bold/italic/links. An unclosed fence renders as a code block so the
 * view stays sane while streaming.
 */

type Block =
  | { kind: "code"; lang: string; text: string }
  | { kind: "heading"; level: number; text: string }
  | { kind: "list"; ordered: boolean; items: string[] }
  | { kind: "quote"; lines: string[] }
  | { kind: "hr" }
  | { kind: "paragraph"; text: string };

const FENCE_RE = /^```(\S*)\s*$/;
const HEADING_RE = /^(#{1,4})\s+(.*)$/;
const UL_RE = /^[-*+]\s+(.*)$/;
const OL_RE = /^\d+[.)]\s+(.*)$/;
const HR_RE = /^(?:-{3,}|\*{3,}|_{3,})\s*$/;

function parseBlocks(src: string): Block[] {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];

    if (!line.trim()) {
      i++;
      continue;
    }

    const fence = line.match(FENCE_RE);
    if (fence) {
      const lang = fence[1] || "";
      const body: string[] = [];
      i++;
      while (i < lines.length && !FENCE_RE.test(lines[i])) {
        body.push(lines[i]);
        i++;
      }
      // Skip the closing fence if present (unclosed = still streaming).
      if (i < lines.length) i++;
      blocks.push({ kind: "code", lang, text: body.join("\n") });
      continue;
    }

    if (HR_RE.test(line)) {
      blocks.push({ kind: "hr" });
      i++;
      continue;
    }

    const heading = line.match(HEADING_RE);
    if (heading) {
      blocks.push({ kind: "heading", level: heading[1].length, text: heading[2] });
      i++;
      continue;
    }

    const ul = line.match(UL_RE);
    const ol = line.match(OL_RE);
    if (ul || ol) {
      const ordered = !!ol;
      const items: string[] = [];
      while (i < lines.length) {
        const cur = lines[i];
        const m = ordered ? cur.match(OL_RE) : cur.match(UL_RE);
        if (!m) break;
        items.push(m[1]);
        i++;
      }
      blocks.push({ kind: "list", ordered, items });
      continue;
    }

    if (line.startsWith(">")) {
      const quoteLines: string[] = [];
      while (i < lines.length && lines[i].startsWith(">")) {
        quoteLines.push(lines[i].replace(/^>\s?/, ""));
        i++;
      }
      blocks.push({ kind: "quote", lines: quoteLines });
      continue;
    }

    const para: string[] = [];
    while (i < lines.length) {
      const cur = lines[i];
      if (
        !cur.trim() ||
        FENCE_RE.test(cur) ||
        HEADING_RE.test(cur) ||
        UL_RE.test(cur) ||
        OL_RE.test(cur) ||
        HR_RE.test(cur) ||
        cur.startsWith(">")
      ) {
        break;
      }
      para.push(cur);
      i++;
    }
    if (para.length) blocks.push({ kind: "paragraph", text: para.join("\n") });
  }

  return blocks;
}

/** Inline token patterns, tried in order at each scan position. */
type InlineToken =
  | { type: "code"; text: string }
  | { type: "bold"; text: string }
  | { type: "italic"; text: string }
  | { type: "link"; text: string; href: string };

function matchInlineToken(text: string, from: number): { token: InlineToken; start: number; end: number } | null {
  for (let i = from; i < text.length; i++) {
    const rest = text.slice(i);
    let m = rest.match(/^`([^`\n]+)`/);
    if (m) return { token: { type: "code", text: m[1] }, start: i, end: i + m[0].length };
    m = rest.match(/^\*\*([^*\n](?:[^*]*?[^*\n])?)\*\*/);
    if (m) return { token: { type: "bold", text: m[1] }, start: i, end: i + m[0].length };
    m = rest.match(/^\*([^*\n]+)\*/);
    if (m) return { token: { type: "italic", text: m[1] }, start: i, end: i + m[0].length };
    m = rest.match(/^\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/);
    if (m) return { token: { type: "link", text: m[1], href: m[2] }, start: i, end: i + m[0].length };
  }
  return null;
}

function renderInline(text: string, keyPrefix: string): ReactNode[] {
  const nodes: ReactNode[] = [];
  let pos = 0;
  let seq = 0;

  while (pos < text.length) {
    const hit = matchInlineToken(text, pos);
    if (!hit) break;
    if (hit.start > pos) {
      nodes.push(text.slice(pos, hit.start));
    }
    const key = `${keyPrefix}-${seq++}`;
    switch (hit.token.type) {
      case "code":
        nodes.push(<code key={key}>{hit.token.text}</code>);
        break;
      case "bold":
        nodes.push(<strong key={key}>{renderInline(hit.token.text, key)}</strong>);
        break;
      case "italic":
        nodes.push(<em key={key}>{renderInline(hit.token.text, key)}</em>);
        break;
      case "link":
        nodes.push(
          <a key={key} href={hit.token.href} target="_blank" rel="noreferrer noopener">
            {hit.token.text}
          </a>
        );
        break;
    }
    pos = hit.end;
  }
  if (pos < text.length) nodes.push(text.slice(pos));
  return nodes;
}

function CodeBlock({ lang, text }: { lang: string; text: string }) {
  const { t } = useTranslation("quickask");
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard unavailable — ignore */
    }
  };

  return (
    <div className="qa-code-block">
      <div className="qa-code-head">
        <span className="qa-code-lang">{lang || "text"}</span>
        <button type="button" className="btn-icon qa-code-copy" onClick={() => void copy()} title={t("copy")}>
          {copied ? <Check size={13} /> : <Copy size={13} />}
        </button>
      </div>
      <pre className="qa-code-body">
        <code>{text}</code>
      </pre>
    </div>
  );
}

export default function MarkdownView({ text }: { text: string }) {
  const blocks = useMemo(() => parseBlocks(text), [text]);

  return (
    <div className="qa-md">
      {blocks.map((b, i) => {
        const key = `b${i}`;
        switch (b.kind) {
          case "code":
            return <CodeBlock key={key} lang={b.lang} text={b.text} />;
          case "heading": {
            const Tag = `h${b.level}` as "h1" | "h2" | "h3" | "h4";
            return <Tag key={key}>{renderInline(b.text, key)}</Tag>;
          }
          case "list":
            return b.ordered ? (
              <ol key={key}>
                {b.items.map((item, j) => (
                  <li key={j}>{renderInline(item, `${key}-${j}`)}</li>
                ))}
              </ol>
            ) : (
              <ul key={key}>
                {b.items.map((item, j) => (
                  <li key={j}>{renderInline(item, `${key}-${j}`)}</li>
                ))}
              </ul>
            );
          case "quote":
            return (
              <blockquote key={key}>
                {b.lines.map((line, j) => (
                  <div key={j}>{renderInline(line, `${key}-${j}`)}</div>
                ))}
              </blockquote>
            );
          case "hr":
            return <hr key={key} />;
          case "paragraph":
            // Single newlines inside a paragraph become <br> — friendlier for
            // CJK answers where markdown-style line merging would jam text.
            return (
              <p key={key}>
                {b.text.split("\n").map((line, j, arr) => (
                  <span key={j}>
                    {renderInline(line, `${key}-${j}`)}
                    {j < arr.length - 1 && <br />}
                  </span>
                ))}
              </p>
            );
        }
      })}
    </div>
  );
}
