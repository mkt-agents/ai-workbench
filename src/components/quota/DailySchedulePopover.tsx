import { useEffect, useRef, useState } from "react";
import { CalendarClock, RefreshCw } from "lucide-react";

export type DailyScheduleFields = {
  enabled: boolean;
  time: string;
  lastRunDate: string | null;
};

export type DailyScheduleSectionCopy = {
  title: string;
  lastRun: (date: string) => string;
  scheduleOff: string;
  hint: string;
  /** WorkBuddy：未配置签到端点时的提示；有值则禁用开关/时间。 */
  notConfigured?: string;
};

export type DailyScheduleSection = {
  id: string;
  settings: DailyScheduleFields | null;
  configured?: boolean;
  copy: DailyScheduleSectionCopy;
  onSave: (enabled: boolean, time: string) => Promise<void>;
  formatError?: (raw: string) => string;
  /** 同组段落连排不画分隔线，组变化时才分隔（如「签到」组 vs「领积分」组）。 */
  group?: string;
};

/** 额度工具条「每日定点」：一段或多段（WB / Trae 签到 / Qoder 领积分）共用一个弹层；`group` 相同的段连排不分隔。 */
export default function DailyJobsPopover({
  sections,
  buttonLabel,
  dialogLabel,
}: {
  sections: DailyScheduleSection[];
  buttonLabel: string;
  dialogLabel: string;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const ready = sections.length > 0 && sections.every((s) => s.settings != null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="aq-pop-anchor" ref={rootRef}>
      <button
        type="button"
        className="btn btn-secondary btn-small"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        disabled={!ready}
      >
        <CalendarClock size={13} />
        {buttonLabel}
      </button>
      {open && ready && (
        <div className="aq-pop aq-pop-jobs" role="dialog" aria-label={dialogLabel}>
          {sections.map((section, i) => (
            <ScheduleSection
              key={section.id}
              section={section}
              divided={i > 0 && section.group !== sections[i - 1].group}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function ScheduleSection({
  section,
  divided,
}: {
  section: DailyScheduleSection;
  divided: boolean;
}) {
  const { settings, configured = true, copy, onSave, formatError } = section;
  const [time, setTime] = useState(settings?.time ?? "09:30");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const enabled = settings?.enabled ?? false;

  useEffect(() => {
    if (settings) setTime(settings.time);
  }, [settings]);

  if (!settings) return null;

  const save = async (nextEnabled: boolean, nextTime: string) => {
    setSaving(true);
    setError(null);
    try {
      await onSave(nextEnabled, nextTime);
    } catch (e) {
      const raw = e instanceof Error ? e.message : String(e);
      setError(formatError ? formatError(raw) : raw);
    } finally {
      setSaving(false);
    }
  };
  const saveTimeIfChanged = () => {
    if (time && time !== settings.time) void save(enabled, time);
  };

  return (
    <div
      className={`aq-pop-section${divided ? " aq-pop-section-divided" : ""}`}
      title={copy.hint}
    >
      <div className="aq-pop-row">
        <label className="aq-pop-check">
          <input
            type="checkbox"
            checked={enabled}
            disabled={saving || !configured}
            onChange={(e) => void save(e.target.checked, time)}
          />
          <span>{copy.title}</span>
        </label>
        <input
          className="input-field aq-pop-time"
          type="time"
          value={time}
          disabled={saving || !configured}
          onChange={(e) => setTime(e.target.value)}
          onBlur={saveTimeIfChanged}
          onKeyDown={(e) => {
            if (e.key === "Enter") saveTimeIfChanged();
          }}
        />
        {saving && <RefreshCw size={12} className="spin" />}
      </div>
      <p className={`aq-pop-status${configured ? "" : " aq-tone-bad"}`}>
        {!configured
          ? (copy.notConfigured ?? copy.scheduleOff)
          : enabled
            ? copy.lastRun(settings.lastRunDate ?? "—")
            : copy.scheduleOff}
      </p>
      {error && (
        <div className="aq-card-error" role="alert">
          {error}
        </div>
      )}
    </div>
  );
}
