/** Light file/code attachments for AI Hub (no RAG). */

export const ASK_ATTACH_MAX_FILES = 3;
/** Total chars across all attachments (names + bodies). */
export const ASK_ATTACH_MAX_CHARS = 12000;
export const ASK_ATTACH_MAX_FILE_CHARS = 8000;

export type AskAttachment = {
  id: string;
  name: string;
  content: string;
  /** Body was cut at ASK_ATTACH_MAX_FILE_CHARS — the chip must say so, not pass silently. */
  truncated: boolean;
  /**
   * 图片附件：`content` 恒为空，模型看到的是真正的 `image` 块（只有标了「支持看图」的
   * 模型收）；base64 不进字符预算，也不拼进 `formatAttachmentsBlock`。
   */
  image?: { mediaType: string; data: string };
};

/** 各家模型服务都认的四种；bmp / tiff 之类先让用户转一下。 */
const IMAGE_MEDIA_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  webp: "image/webp",
  gif: "image/gif",
};

/** `image/png` 之类；不是受支持的图片时 null。 */
export function imageMediaTypeOf(path: string): string | null {
  const name = attachmentNameFromPath(path).toLowerCase();
  const dot = name.lastIndexOf(".");
  if (dot < 0) return null;
  return IMAGE_MEDIA_TYPES[name.slice(dot + 1)] ?? null;
}

export function isImageAttachmentPath(path: string): boolean {
  return imageMediaTypeOf(path) !== null;
}

/** Plain-text-ish suffixes; anything else would reach the model as mojibake. */
const TEXT_SUFFIXES = [
  "txt", "md", "markdown", "json", "jsonc", "yaml", "yml", "toml", "ini", "conf", "cfg", "log",
  "csv", "tsv", "xml", "html", "htm", "css", "scss", "less", "ts", "tsx", "js", "jsx", "mjs",
  "cjs", "vue", "svelte", "rs", "py", "go", "java", "kt", "cs", "c", "h", "cpp", "cc", "hpp",
  "php", "rb", "swift", "sh", "ps1", "bat", "cmd", "sql", "properties", "env",
];

export function attachmentNameFromPath(path: string): string {
  const base = path.split(/[/\\]/).filter(Boolean).pop() ?? "";
  return base || "file.txt";
}

export function isTextAttachmentPath(path: string): boolean {
  const name = attachmentNameFromPath(path).toLowerCase();
  const dot = name.lastIndexOf(".");
  if (dot < 0) return false;
  return TEXT_SUFFIXES.includes(name.slice(dot + 1));
}

export function attachmentChars(list: AskAttachment[]): number {
  return list.reduce((n, a) => n + a.name.length + a.content.length, 0);
}

export function canAddAttachment(
  list: AskAttachment[],
  next: { name: string; content: string }
): { ok: true } | { ok: false; reason: "count" | "size" } {
  if (list.length >= ASK_ATTACH_MAX_FILES) return { ok: false, reason: "count" };
  const body = next.content.slice(0, ASK_ATTACH_MAX_FILE_CHARS);
  if (attachmentChars(list) + next.name.length + body.length > ASK_ATTACH_MAX_CHARS) {
    return { ok: false, reason: "size" };
  }
  return { ok: true };
}

export function makeAttachment(name: string, content: string): AskAttachment {
  const trimmed = content.slice(0, ASK_ATTACH_MAX_FILE_CHARS);
  const base = name.trim() || "snippet.txt";
  return {
    id: `att-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
    name: base,
    content: trimmed,
    truncated: content.length > trimmed.length,
  };
}

export function makeImageAttachment(
  name: string,
  image: { mediaType: string; data: string }
): AskAttachment {
  return {
    id: `att-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
    name: name.trim() || "image.png",
    content: "",
    truncated: false,
    image,
  };
}

/** 图片附件单独取出：它们以 `image` 块随消息发出，不参与文本拼装。 */
export function imageAttachmentsOf(list: AskAttachment[]): { mediaType: string; data: string }[] {
  return list.flatMap((a) => (a.image ? [a.image] : []));
}

/** Append attachments as a fenced block after the user question. */
export function formatAttachmentsBlock(list: AskAttachment[]): string {
  // 图片走真正的 image 块，这里只拼文本
  const texts = list.filter((a) => !a.image);
  if (!texts.length) return "";
  const parts = texts.map(
    (a) =>
      `### ${a.name}${
        a.truncated ? ` (truncated — only the first ${ASK_ATTACH_MAX_FILE_CHARS} characters)` : ""
      }\n\`\`\`\n${a.content}\n\`\`\``
  );
  return `\n\n---\nAttached files/code from the user:\n${parts.join("\n\n")}`;
}
