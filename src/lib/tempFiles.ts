/**
 * 疑似临时/非项目文件识别：Git 提交页打标 + 提交前确认共用。
 * 只匹配高置信模式（编辑器/系统产物），宁缺勿滥——识别结果只是警告，最终由用户决定。
 */
const TEMP_EXTENSIONS = new Set([
  "tmp",
  "temp",
  "bak",
  "swp",
  "swo",
  "swn",
  "log",
  "orig",
  "rej",
  "stackdump",
]);

const TEMP_BASENAMES = new Set([".ds_store", "thumbs.db", "desktop.ini", "nul"]);

export function looksLikeTempFile(path: string): boolean {
  const normalized = path.replace(/\\/g, "/");
  const name = normalized.slice(normalized.lastIndexOf("/") + 1).toLowerCase();
  if (!name) return false;
  if (TEMP_BASENAMES.has(name)) return true;
  // Office 锁文件 ~$xxx、本工具/代理产生的 .tmp-xxx 草稿
  if (name.startsWith("~$") || name.startsWith(".tmp-")) return true;
  // 编辑器备份 xxx~
  if (name.endsWith("~")) return true;
  const dot = name.lastIndexOf(".");
  if (dot > 0 && TEMP_EXTENSIONS.has(name.slice(dot + 1))) return true;
  return false;
}
