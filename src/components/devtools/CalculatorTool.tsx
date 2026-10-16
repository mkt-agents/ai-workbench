import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Copy, Delete, Eraser } from "lucide-react";
import { useGlobalStore } from "../../core/store";

/**
 * 安全表达式求值：递归下降，只支持 + - × ÷ % 和括号，不使用 eval。
 * % 是后缀百分数（50% = 0.5），除以 0 抛 "div0"。
 */
export function evaluateExpression(input: string): number {
  const src = input.replace(/×/g, "*").replace(/÷/g, "/").replace(/−/g, "-").trim();
  if (!src) throw new Error("empty");
  let pos = 0;
  const skip = () => {
    while (pos < src.length && src[pos] === " ") pos++;
  };
  const peek = () => {
    skip();
    return pos < src.length ? src[pos] : "";
  };
  const parseExpr = (): number => {
    let v = parseTerm();
    for (;;) {
      const c = peek();
      if (c === "+") {
        pos++;
        v += parseTerm();
      } else if (c === "-") {
        pos++;
        v -= parseTerm();
      } else return v;
    }
  };
  const parseTerm = (): number => {
    let v = parseFactor();
    for (;;) {
      const c = peek();
      if (c === "*") {
        pos++;
        v *= parseFactor();
      } else if (c === "/") {
        pos++;
        const d = parseFactor();
        if (d === 0) throw new Error("div0");
        v /= d;
      } else return v;
    }
  };
  const parseFactor = (): number => {
    let v = parseUnary();
    while (peek() === "%") {
      pos++;
      v /= 100;
    }
    return v;
  };
  const parseUnary = (): number => {
    if (peek() === "-") {
      pos++;
      return -parseUnary();
    }
    if (peek() === "+") {
      pos++;
      return parseUnary();
    }
    return parsePrimary();
  };
  const parsePrimary = (): number => {
    skip();
    if (src[pos] === "(") {
      pos++;
      const v = parseExpr();
      if (peek() !== ")") throw new Error("paren");
      pos++;
      return v;
    }
    const start = pos;
    while (pos < src.length && /[0-9.]/.test(src[pos])) pos++;
    if (start === pos) throw new Error("token");
    const n = Number(src.slice(start, pos));
    if (!Number.isFinite(n)) throw new Error("num");
    return n;
  };

  const v = parseExpr();
  skip();
  if (pos < src.length) throw new Error("trail");
  return v;
}

/** 12 位有效数字取整，消掉 0.1+0.2 之类的浮点尾噪。 */
function formatNumber(n: number): string {
  if (!Number.isFinite(n)) return String(n);
  return String(parseFloat(n.toPrecision(12)));
}

type CalcKey = {
  label: string;
  kind: "util" | "num" | "op";
  insert?: string;
  action?: "clear" | "back";
};

const KEYS: CalcKey[] = [
  { label: "C", kind: "util", action: "clear" },
  { label: "⌫", kind: "util", action: "back" },
  { label: "(", kind: "op", insert: "(" },
  { label: ")", kind: "op", insert: ")" },
  { label: "7", kind: "num", insert: "7" },
  { label: "8", kind: "num", insert: "8" },
  { label: "9", kind: "num", insert: "9" },
  { label: "÷", kind: "op", insert: "÷" },
  { label: "4", kind: "num", insert: "4" },
  { label: "5", kind: "num", insert: "5" },
  { label: "6", kind: "num", insert: "6" },
  { label: "×", kind: "op", insert: "×" },
  { label: "1", kind: "num", insert: "1" },
  { label: "2", kind: "num", insert: "2" },
  { label: "3", kind: "num", insert: "3" },
  { label: "-", kind: "op", insert: "-" },
  { label: "0", kind: "num", insert: "0" },
  { label: ".", kind: "num", insert: "." },
  { label: "%", kind: "op", insert: "%" },
  { label: "+", kind: "op", insert: "+" },
];

const HISTORY_MAX = 20;

function CalculatorTool() {
  const { t } = useTranslation("devtools");
  const copy = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [expr, setExpr] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [history, setHistory] = useState<{ expr: string; result: string }[]>([]);
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // 挂载即聚焦：数字直接进表达式，不会误触工具条的 1-9 切换
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // 表达式实时预览；输入未完成（如 "5+"）时静默，错误只在按 = 时提示
  const preview = useMemo(() => {
    const trimmed = expr.trim();
    if (!trimmed) return null;
    try {
      return `= ${formatNumber(evaluateExpression(trimmed))}`;
    } catch {
      return null;
    }
  }, [expr]);

  const update = (next: string) => {
    setExpr(next);
    setError(null);
  };

  const pressKey = (key: CalcKey) => {
    if (key.action === "clear") update("");
    else if (key.action === "back") update(expr.slice(0, -1));
    else if (key.insert) update(expr + key.insert);
  };

  const commit = () => {
    const trimmed = expr.trim();
    if (!trimmed) return;
    try {
      const result = formatNumber(evaluateExpression(trimmed));
      setHistory((prev) => [{ expr: trimmed, result }, ...prev].slice(0, HISTORY_MAX));
      update(result);
    } catch (e) {
      setError(e instanceof Error && e.message === "div0" ? t("calc.divZero") : t("calc.error"));
    }
  };

  const copyResult = async (text: string, idx: number) => {
    try {
      await copy(text);
      setCopiedIdx(idx);
      setTimeout(() => setCopiedIdx(null), 1200);
    } catch {
      /* 剪贴板失败静默 */
    }
  };

  return (
    <div className="devtools-tool calc-tool">
      <div className="calc-layout">
        <div className="calc-panel">
          <div className={`calc-display${error ? " has-error" : ""}`}>
            <input
              ref={inputRef}
              className="calc-input"
              value={expr}
              onChange={(e) => update(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  commit();
                } else if (e.key === "Escape") {
                  e.preventDefault();
                  update("");
                }
              }}
              placeholder="0"
              spellCheck={false}
              autoComplete="off"
              aria-label={t("calc.title")}
            />
            <div className={`calc-preview${error ? " is-error" : ""}`}>
              {error ?? preview ?? "\u00a0"}
            </div>
          </div>

          <div className="calc-pad">
            {KEYS.map((key) => (
              <button
                key={key.label}
                type="button"
                className={`calc-key ${key.kind}`}
                // 不抢焦点：表达式输入框保持聚焦，键盘输入不中断
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pressKey(key)}
              >
                {key.action === "back" ? <Delete size={15} /> : key.label}
              </button>
            ))}
            <button
              type="button"
              className="calc-key equals"
              onMouseDown={(e) => e.preventDefault()}
              onClick={commit}
            >
              =
            </button>
          </div>
          <p className="calc-hint">{t("calc.percentHint")}</p>
        </div>

        <div className="calc-history">
          <div className="calc-history-head">
            <span className="calc-history-title">{t("calc.history")}</span>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => setHistory([])}
              disabled={history.length === 0}
              title={t("calc.clearHistory")}
            >
              <Eraser size={12} />
              {t("calc.clearHistory")}
            </button>
          </div>
          {history.length === 0 ? (
            <div className="calc-history-empty">{t("calc.historyEmpty")}</div>
          ) : (
            <div className="calc-history-list">
              {history.map((item, i) => (
                <div
                  key={`${item.expr}-${i}`}
                  className="calc-history-item"
                  onClick={() => update(item.expr)}
                  title={item.expr}
                >
                  <span className="calc-history-expr">{item.expr}</span>
                  <span className="calc-history-result">= {item.result}</span>
                  <button
                    type="button"
                    className="calc-history-copy"
                    onClick={(e) => {
                      e.stopPropagation();
                      void copyResult(item.result, i);
                    }}
                    title={t("calc.copy")}
                  >
                    {copiedIdx === i ? <Check size={12} /> : <Copy size={12} />}
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

export default CalculatorTool;
