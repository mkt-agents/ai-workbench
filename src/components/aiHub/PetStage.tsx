import { useEffect, useState } from "react";
import { petClipFor, petGestureFor, type PetMood, type PetSkinDef } from "../../lib/petSkins";
import type { BondTier } from "../../lib/petBond";
import { petalCount } from "../../lib/petBond";
import type { PetGesturePlay } from "../../lib/petBrain";
import PandaGirlPet, { type PetMicro, type PressZone } from "./PandaGirlPet";

/** 交互阶段（按下/拖拽/发射）与情绪是两条独立轴，CSS 各管一层。 */
export type BubblePhase = "idle" | "pressed" | "dragging" | "firing" | "landing";

export type { PressZone };

/**
 * 眨眼每一格的时长（ms）。素材是 12fps，但眨眼要比常规帧再快一档。
 */
const BLINK_STEP_MS = 68;
const BLINK_GAP_MIN_MS = 2400;
const BLINK_GAP_SPAN_MS = 5000;
const DOUBLE_BLINK_CHANCE = 0.16;
const DOUBLE_BLINK_GAP_MS = 120;

interface Props {
  skin: PetSkinDef;
  mood: PetMood;
  phase: BubblePhase;
  bondTier?: BondTier;
  gesture?: PetGesturePlay | null;
  micro?: PetMicro;
  celebrate?: boolean;
  /** 按下分区：头/身/裙边不同压感 */
  pressZone?: PressZone | null;
}

/**
 * 桌宠舞台：把情绪演出来。
 *
 * 三分支：vector（小肥鱼 SVG）/ orb（水晶圆盘）/ sprite（PNG·WebM 自备包）。
 */
export default function PetStage({
  skin,
  mood,
  phase,
  bondTier = 0,
  gesture = null,
  micro = "none",
  celebrate = false,
  pressZone = null,
}: Props) {
  if (skin.kind === "vector") {
    return (
      <div className={`qa-pet mood-${mood} is-${phase}`} data-skin={skin.id}>
        <PandaGirlPet
          mood={mood}
          phase={phase}
          bondTier={bondTier}
          gesture={gesture}
          micro={micro}
          celebrate={celebrate}
          pressZone={pressZone}
        />
        <PetFx
          mood={mood}
          celebrate={celebrate}
          bondTier={bondTier}
          gesture={gesture}
          micro={micro}
          phase={phase}
        />
      </div>
    );
  }

  return (
    <SpriteStage skin={skin} mood={mood} phase={phase} celebrate={celebrate} bondTier={bondTier} />
  );
}

function SpriteStage({
  skin,
  mood,
  phase,
  celebrate,
  bondTier,
}: {
  skin: PetSkinDef;
  mood: PetMood;
  phase: BubblePhase;
  celebrate: boolean;
  bondTier: BondTier;
}) {
  const gestureName = phase === "dragging" ? "drag" : phase === "firing" ? "tap" : null;
  const clip =
    (gestureName ? petGestureFor(skin, gestureName) : undefined) ?? petClipFor(skin, mood);
  const [index, setIndex] = useState(0);
  const [blinkStep, setBlinkStep] = useState(-1);

  useEffect(() => {
    setIndex(0);
    if (!clip || clip.fps <= 0 || clip.frames.length < 2) return;
    const step = Math.max(16, Math.round(1000 / clip.fps));
    const timer = window.setInterval(
      () => setIndex((i) => (i + 1) % clip.frames.length),
      step,
    );
    return () => window.clearInterval(timer);
  }, [clip]);

  useEffect(() => {
    const frames = skin.blink;
    setBlinkStep(-1);
    if (!frames?.length || mood !== "idle" || phase !== "idle") return;
    let timer = 0;
    let cancelled = false;

    const blinkOnce = (done: () => void) => {
      let at = 0;
      const step = () => {
        if (cancelled) return;
        if (at >= frames.length) {
          setBlinkStep(-1);
          done();
          return;
        }
        setBlinkStep(at);
        at += 1;
        timer = window.setTimeout(step, BLINK_STEP_MS);
      };
      step();
    };

    const schedule = () => {
      timer = window.setTimeout(() => {
        if (cancelled) return;
        blinkOnce(() => {
          if (cancelled) return;
          if (Math.random() < DOUBLE_BLINK_CHANCE) {
            timer = window.setTimeout(() => {
              if (!cancelled) blinkOnce(schedule);
            }, DOUBLE_BLINK_GAP_MS);
          } else {
            schedule();
          }
        });
      }, BLINK_GAP_MIN_MS + Math.random() * BLINK_GAP_SPAN_MS);
    };

    schedule();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [skin, mood, phase]);

  const baseFrame = clip?.frames.length ? clip.frames[index % clip.frames.length] : undefined;
  const blinking = blinkStep >= 0 ? skin.blink?.[blinkStep] : undefined;
  const frame = blinking ?? baseFrame;
  const bare = skin.kind === "sprite" && !frame;

  return (
    <div className={`qa-pet mood-${mood} is-${phase}`} data-skin={skin.id}>
      {skin.kind === "orb" || bare ? (
        <div className="qa-bubble-orb">
          <BubbleMark className="qa-bubble-icon" />
        </div>
      ) : (
        <>
          <span className="qa-pet-shadow" aria-hidden />
          <div className="qa-pet-body">
            <div className="qa-pet-sway">
              {clip?.video ? (
                <video
                  key={frame}
                  className="qa-pet-vid"
                  src={frame}
                  autoPlay
                  loop
                  muted
                  playsInline
                  disablePictureInPicture
                  aria-hidden
                />
              ) : (
                frame && <img className="qa-pet-img" src={frame} alt="" draggable={false} />
              )}
            </div>
          </div>
          {!clip?.video && <PetFx mood={mood} celebrate={celebrate} bondTier={bondTier} />}
        </>
      )}
    </div>
  );
}

/** 情绪特效层：只有 transform / opacity，窗口拖动时不重排。 */
function PetFx({
  mood,
  celebrate,
  bondTier,
  gesture = null,
  micro = "none",
  phase = "idle",
}: {
  mood: PetMood;
  celebrate: boolean;
  bondTier: BondTier;
  gesture?: PetGesturePlay | null;
  micro?: PetMicro;
  phase?: BubblePhase;
}) {
  // 被甩晕 / 被拎久挣扎：优先于拖拽特效，否则拖拽的星星爱心会抢掉演出
  if (micro === "dizzy") {
    return (
      <span className="qa-fx qa-fx-dizzy" aria-hidden>
        <i>✦</i>
        <i>✦</i>
        <i>✦</i>
      </span>
    );
  }
  if (micro === "struggle") {
    return (
      <span className="qa-fx qa-fx-sweat" aria-hidden>
        <i />
        <i />
      </span>
    );
  }
  // 拖动：小星星 + 爱心，像被抱着走
  if (phase === "dragging") {
    return (
      <>
        <span className="qa-fx qa-fx-spark" aria-hidden>
          <i />
          <i />
          <i />
        </span>
        <span className="qa-fx qa-fx-heart" aria-hidden>
          ♥
        </span>
      </>
    );
  }
  // 害羞：冷汗（腮红在 pet-panda-girl.css）；别跟开心爱心叠
  if (gesture === "shy" || micro === "shy") {
    return (
      <span className="qa-fx qa-fx-sweat" aria-hidden>
        <i />
        <i />
      </span>
    );
  }
  // 摸头/抱抱手势：即使 mood 尚未切到 happy，也先给一点爱心反馈
  if (gesture === "pat" || gesture === "hug") {
    return (
      <>
        <span className="qa-fx qa-fx-spark" aria-hidden>
          <i />
          <i />
          <i />
        </span>
        <span className="qa-fx qa-fx-heart" aria-hidden>
          ♥
        </span>
        {gesture === "hug" && (
          <span className="qa-fx qa-fx-heart qa-fx-heart-2" aria-hidden>
            ♥
          </span>
        )}
      </>
    );
  }
  if (mood === "thinking") {
    return (
      <span className="qa-fx qa-fx-dots" aria-hidden>
        <i />
        <i />
        <i />
      </span>
    );
  }
  if (mood === "working") {
    return (
      <span className="qa-fx qa-fx-orbit qa-fx-orbit-busy" aria-hidden>
        <i />
        <i />
        <i />
      </span>
    );
  }
  if (mood === "waiting") {
    return (
      <>
        <span className="qa-fx qa-fx-knock" aria-hidden>
          ?
        </span>
        <span className="qa-fx qa-fx-sweat" aria-hidden>
          <i />
          <i />
        </span>
      </>
    );
  }
  if (mood === "happy") {
    const n = celebrate ? petalCount(bondTier) : 0;
    return (
      <>
        <span className="qa-fx qa-fx-spark" aria-hidden>
          <i />
          <i />
          <i />
          <i />
        </span>
        <span className="qa-fx qa-fx-heart" aria-hidden>
          ♥
        </span>
        {celebrate && (
          <span className="qa-fx qa-fx-heart qa-fx-heart-2" aria-hidden>
            ♥
          </span>
        )}
        {n > 0 && (
          <span className="qa-fx qa-fx-petals" aria-hidden>
            {Array.from({ length: n }, (_, i) => (
              <i key={i} style={{ ["--p" as string]: String(i) }} />
            ))}
          </span>
        )}
      </>
    );
  }
  if (mood === "error") {
    return <span className="qa-fx qa-fx-cloud" aria-hidden />;
  }
  if (mood === "sleepy") {
    return (
      <span className="qa-fx qa-fx-zzz" aria-hidden>
        z
      </span>
    );
  }
  return null;
}

function BubbleMark({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      width="17"
      height="17"
      viewBox="0 0 18 18"
      fill="none"
      aria-hidden
    >
      <path
        d="M9 1.6c.28 2.85 1.75 4.32 4.6 4.6-2.85.28-4.32 1.75-4.6 4.6-.28-2.85-1.75-4.32-4.6-4.6 2.85-.28 4.32-1.75 4.6-4.6Z"
        stroke="currentColor"
        strokeWidth="1.25"
        strokeLinejoin="round"
      />
      <circle cx="14.2" cy="3.6" r="0.85" fill="currentColor" opacity="0.85" />
      <circle cx="3.9" cy="13.8" r="0.65" fill="currentColor" opacity="0.55" />
    </svg>
  );
}
