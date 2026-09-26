import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Radar } from "lucide-react";
import {
  wbCbLoginSelftest,
  wbCloseCbLoginWindow,
  wbOpenCbLoginWindow,
  wbReadCbLoginProbe,
  wbUpdateSettings,
  type WbProbeCall,
  type WbProbeReport,
  type WbSelfTestOutcome,
  type WbSettings,
} from "../../lib/workbuddy";
import ModalTitleRow from "../ModalTitleRow";
import { useInvokeErrorTranslator } from "../../hooks/useInvokeError";

interface Props {
  settings: WbSettings;
  onClose: () => void;
  onSaved: (next: WbSettings) => void;
}

type ProbeTarget = "credentialProbe" | "checkin" | "chat";

const TARGETS: { key: ProbeTarget; label: string }[] = [
  { key: "credentialProbe", label: "probeTargetProbe" },
  { key: "checkin", label: "probeTargetCheckin" },
  { key: "chat", label: "probeTargetChat" },
];

function newProbeBatch() {
  return Math.random().toString(36).slice(2, 12);
}

/**
 * 探针条目 → 端点配置。
 *
 * 认证头是关键：页面自己带了 authorization 就照抄（值用 {token}），什么都没带
 * 说明是浏览器自动送的会话 Cookie（值用 {cookie}）——这正是判凭证类型的依据。
 */
function endpointFromCall(call: WbProbeCall, target: ProbeTarget) {
  const names = call.a.split(",").map((n) => n.trim()).filter(Boolean);
  const headers: [string, string][] = names.length
    ? names.map((n) => [n, n === "cookie" ? "{cookie}" : "Bearer {token}"] as [string, string])
    : [["cookie", "{cookie}"]];
  const method = (target === "credentialProbe" ? "GET" : call.m || "POST").toUpperCase();
  const endpoint: Record<string, unknown> = { method, url: call.u, headers };
  if (method !== "GET") endpoint.body = target === "chat" ? '{"model":"{model}"}' : "{}";
  return endpoint;
}

function pathOf(url: string) {
  const i = url.indexOf("://");
  const rest = i >= 0 ? url.slice(i + 3) : url;
  const slash = rest.indexOf("/");
  return slash >= 0 ? rest.slice(slash) : "/";
}

/** 3xx 是登录页最常见的「还没登录」回包（302 到 Keycloak），拿来探测会误判，所以标灰而不是标绿。 */
function statusTone(code: number) {
  if (code >= 500) return "bad";
  if (code === 401 || code === 403) return "warn";
  if (code >= 400) return "bad";
  if (code >= 300) return "muted";
  return code >= 200 ? "ok" : "muted";
}

const TEMPLATE = JSON.stringify(
  {
    // 2026-09-26 实测：platform 决定登录完成后交给谁——admin=WorkBuddy 企业版管理后台
    // （没有企业租户会卡在「暂无企业账号」）、workbuddy=桌面客户端回传通道（网页扫码必
    // 报「登录失败，请返回客户端重新发起登录」）、usercenter=个人版个人中心。带上
    // redirect_uri 才会停在窗口内，会话 cookie 于是留在我们的 webview 里。
    loginUrl:
      "https://www.codebuddy.cn/login/?platform=usercenter&state=0&redirect_uri=https%3A%2F%2Fwww.codebuddy.cn%2Fprofile%2Fplan",
    // 凭证 = 扫码登录捕获的 Keycloak JWT（token 类型，body:/console/login/enterprise 抓到）。
    // 探测端点实测三态：带真 JWT → 200；不带或带 Cookie → 401（Bearer 门控）；缺
    // user_enterprise_id=personal 参数 → 400。200 即凭证活着，GET 不烧 token。
    credentialProbe: {
      method: "GET",
      url: "https://www.codebuddy.cn/console/api/client/v1/api-keys?page=1&page_size=10&user_enterprise_id=personal",
      headers: [
        ["authorization", "Bearer {token}"],
        ["x-client-platform", "web"],
      ],
    },
    // 签到 = WorkBuddy 客户端「Buddy加油站」的每日领积分（2026-09-26 真 JWT 实测：
    // 真签到回 {"code":0,"data":{"credit":100,"streak_days":10}}，无凭证 401）。
    // 端点挖自 WorkBuddy 客户端 app.asar（httpService 全部走 copilot.tencent.com）。
    checkin: {
      method: "POST",
      url: "https://copilot.tencent.com/billing/meter/daily-checkin",
      headers: [
        ["authorization", "Bearer {token}"],
        ["content-type", "application/json"],
      ],
      body: "{}",
    },
    // 签到状态查询（只读）：今日是否已签 / 连登 / 周进度 / 积分余额。
    // 同族端点，同样挖自 app.asar；失败不影响账号状态与签到连败计数。
    checkinStatus: {
      method: "POST",
      url: "https://copilot.tencent.com/billing/meter/checkin-status",
      headers: [
        ["authorization", "Bearer {token}"],
        ["content-type", "application/json"],
      ],
      body: "{}",
    },
    // 成长计划（只读展示，不自动兑换）：连登阶梯与任务列表。
    // codebuddy.cn 同源 usercenter 族端点，也认 Bearer JWT；未配置时面板显示灰态。
    growthStreak: {
      method: "GET",
      url: "https://www.codebuddy.cn/activity/growth/streak",
      headers: [["authorization", "Bearer {token}"]],
    },
    growthTasks: {
      method: "GET",
      url: "https://www.codebuddy.cn/v2/activity/growth/tasks",
      headers: [["authorization", "Bearer {token}"]],
    },
    // 实测 2026-09-26（IDE genie 扩展同款端点）：真 JWT → 400 {code:11101 "Non-stream
    // chat request is currently not supported"} = 鉴权已过；无/假凭证 → 401；
    // tokenhub.tencentmaas.com 对 JWT 一律 401（那是云 API Key 的入口）。
    // ⚠️ 上游只支持流式：客户端必须 stream:true，否则会拿到 11101 错误。
    chat: {
      method: "POST",
      url: "https://copilot.tencent.com/v2/chat/completions",
      headers: [
        ["authorization", "Bearer {token}"],
        ["content-type", "application/json"],
      ],
    },
    models: null,
    fallbackModels: ["deepseek-chat"],
  },
  null,
  2
);

/** 协议编辑器：登录/签到/上游端点全在这里，抓包确认后粘贴 JSON 即可，不用重新构建。 */
export default function ProtocolPanel({ settings, onClose, onSaved }: Props) {
  const { t } = useTranslation("workbuddy");
  const translateError = useInvokeErrorTranslator();
  const [json, setJson] = useState(settings.adapterJson?.trim() || "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const batchRef = useRef(newProbeBatch());
  const openedRef = useRef(false);
  const [windowOpen, setWindowOpen] = useState(false);
  const [report, setReport] = useState<WbProbeReport | null>(null);
  const [probeBusy, setProbeBusy] = useState(false);
  const [probeError, setProbeError] = useState<string | null>(null);
  const [applied, setApplied] = useState<ProbeTarget | null>(null);
  const [testing, setTesting] = useState<string | null>(null);
  const [selfTest, setSelfTest] = useState<{ call: WbProbeCall; outcome: WbSelfTestOutcome } | null>(null);

  // 探测窗是本组件自己开的，关掉面板时别把它留成孤儿窗口。
  useEffect(
    () => () => {
      if (openedRef.current) void wbCloseCbLoginWindow(batchRef.current).catch(() => undefined);
    },
    []
  );

  const openProbeWindow = useCallback(async () => {
    setProbeBusy(true);
    setProbeError(null);
    try {
      await wbOpenCbLoginWindow(batchRef.current);
      openedRef.current = true;
      setWindowOpen(true);
    } catch (e) {
      setProbeError(translateError(e));
    } finally {
      setProbeBusy(false);
    }
  }, [translateError]);

  const readProbe = useCallback(async () => {
    setProbeBusy(true);
    setProbeError(null);
    try {
      const next = await wbReadCbLoginProbe(batchRef.current);
      setReport(next);
      if (next.empty) setProbeError(t("probeEmpty"));
    } catch (e) {
      setProbeError(translateError(e));
    } finally {
      setProbeBusy(false);
    }
  }, [t, translateError]);

  const closeProbeWindow = useCallback(() => {
    void wbCloseCbLoginWindow(batchRef.current).catch(() => undefined);
    openedRef.current = false;
    setWindowOpen(false);
  }, []);

  // 让页面自己再打一次这个请求：通了说明端点选对了，凭证却带不出去；
  // 照样 401 说明连端点都不对（或这个页面根本不调它）。
  const runSelfTest = async (call: WbProbeCall) => {
    setTesting(call.u);
    setSelfTest(null);
    try {
      setSelfTest({ call, outcome: await wbCbLoginSelftest(batchRef.current, call.m, call.u) });
    } catch (e) {
      setProbeError(translateError(e));
    } finally {
      setTesting(null);
    }
  };

  const applyTo = (target: ProbeTarget, call: WbProbeCall) => {
    let base: Record<string, unknown> = {};
    if (json.trim()) {
      try {
        const parsed: unknown = JSON.parse(json);
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) base = parsed as Record<string, unknown>;
      } catch {
        /* 编辑器里是坏 JSON：以探针结果为起点重建，用户反正得重填 */
      }
    }
    base[target] = endpointFromCall(call, target);
    setJson(JSON.stringify(base, null, 2));
    setError(null);
    setApplied(target);
  };

  const parseState = useMemo(() => {
    if (!json.trim()) return null;
    try {
      const parsed: unknown = JSON.parse(json);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
        return t("protocolInvalidJson", { message: "expect object" });
      }
      return null;
    } catch (e) {
      return translateError(e);
    }
  }, [json, t, translateError]);

  const save = async () => {
    if (parseState) {
      setError(parseState);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      onSaved(await wbUpdateSettings({ adapterJson: json.trim() }));
    } catch (e) {
      setError(translateError(e));
      setBusy(false);
    }
  };

  return (
    <div className="modal-overlay" onClick={busy ? undefined : onClose}>
      <div className="modal wb-modal wb-modal-wide" onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow title={t("protocolTitle")} onClose={onClose} disabled={busy} />

        <section className="wb-probe">
          <div className="wb-probe-bar">
            <Radar size={14} />
            <span className="wb-probe-title">{t("probeTitle")}</span>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => void openProbeWindow()}
              disabled={probeBusy || windowOpen}
            >
              {t("probeOpen")}
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => void readProbe()}
              disabled={probeBusy || !windowOpen}
            >
              {probeBusy && <Loader2 size={12} className="spin" />}
              <span>{t("probeRead")}</span>
            </button>
            {windowOpen && (
              <button type="button" className="btn btn-secondary btn-small" onClick={closeProbeWindow}>
                {t("probeClose")}
              </button>
            )}
          </div>
          <p className="wb-hint">{t(windowOpen ? "probeReadHint" : "probeHint")}</p>
          {probeError && <div className="wb-inline-error" role="alert">{probeError}</div>}
          {applied && !probeError && (
            <p className="wb-probe-applied">{t("probeApplied", { target: applied })}</p>
          )}
          {report && !report.empty && (
            <div className="wb-probe-scroll">
              <table className="wb-table wb-probe-table">
                <colgroup>
                  <col className="wb-col-m" />
                  <col />
                  <col className="wb-col-s" />
                  <col className="wb-col-auth" />
                  <col className="wb-col-n" />
                  <col className="wb-col-set" />
                </colgroup>
                <thead>
                  <tr>
                    <th>{t("probeColMethod")}</th>
                    <th>{t("probeColPath")}</th>
                    <th>{t("probeColStatus")}</th>
                    <th>{t("probeColAuth")}</th>
                    <th className="wb-num">{t("probeColCount")}</th>
                    <th>{t("probeColSet")}</th>
                  </tr>
                </thead>
                <tbody>
                  {report.calls.map((call) => (
                    <tr key={`${call.m} ${call.u} ${call.s}`}>
                      <td className="mono">{call.m}</td>
                      <td className="wb-probe-url" title={call.u}>
                        <span className="mono">{pathOf(call.u)}</span>
                      </td>
                      <td>
                        <span className={`wb-probe-code is-${statusTone(call.s)}`}>{call.s || "—"}</span>
                      </td>
                      <td className="wb-probe-auth" title={call.a}>
                        <span className="mono">{call.a || "—"}</span>
                      </td>
                      <td className="wb-num mono">{call.n}</td>
                      <td>
                        <div className="wb-probe-actions">
                          <button
                            type="button"
                            className="btn btn-secondary btn-small"
                            onClick={() => void runSelfTest(call)}
                            disabled={!windowOpen || testing !== null}
                            title={t("probeTryHint")}
                          >
                            {testing === call.u && <Loader2 size={11} className="spin" />}
                            <span>{t("probeTry")}</span>
                          </button>
                          {TARGETS.map((target) => (
                            <button
                              key={target.key}
                              type="button"
                              className="btn btn-secondary btn-small"
                              onClick={() => applyTo(target.key, call)}
                            >
                              {t(target.label)}
                            </button>
                          ))}
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {selfTest && (
            <p className="wb-probe-selftest">
              <span className={`wb-probe-code is-${statusTone(selfTest.outcome.status)}`}>
                {selfTest.outcome.status || "—"}
              </span>
              <span className="mono">
                {selfTest.call.m} {pathOf(selfTest.outcome.url || selfTest.call.u)}
              </span>
              <span className="wb-probe-selftest-body mono">
                {selfTest.outcome.error ?? selfTest.outcome.bodyHead}
              </span>
            </p>
          )}
        </section>

        <p className="wb-hint">{t("protocolHint")}</p>
        <ul className="wb-doc-list">
          <li>{t("protocolDocProbe")}</li>
          <li>{t("protocolDocCheckin")}</li>
          <li>{t("protocolDocChat")}</li>
        </ul>
        <div className="input-group">
          <textarea
            className="input-field wb-json-input mono"
            rows={16}
            value={json}
            onChange={(e) => setJson(e.target.value)}
            placeholder={TEMPLATE}
            disabled={busy}
            spellCheck={false}
          />
        </div>
        {!json.trim() && (
          <button type="button" className="btn btn-secondary btn-small" onClick={() => setJson(TEMPLATE)} disabled={busy}>
            {t("protocolUseTemplate")}
          </button>
        )}
        {error && <div className="wb-inline-error" role="alert">{error}</div>}
        {!error && parseState && <div className="wb-inline-error" role="alert">{parseState}</div>}
        <div className="modal-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose} disabled={busy}>
            {t("cancel")}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => void save()}
            disabled={busy || Boolean(parseState)}
          >
            {busy && <Loader2 size={13} className="spin" />}
            <span>{t("protocolApply")}</span>
          </button>
        </div>
      </div>
    </div>
  );
}
