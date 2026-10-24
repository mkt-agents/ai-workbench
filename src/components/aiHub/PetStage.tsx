import { useEffect, useState } from "react";
import { petClipFor, petGestureFor, type PetMood, type PetSkinDef } from "../../lib/petSkins";

/** 交互阶段（按下/拖拽/发射）与情绪是两条独立轴，CSS 各管一层。 */
export type BubblePhase = "idle" | "pressed" | "dragging" | "firing" | "landing";

/**
 * 眨眼每一格的时长（ms）。素材是 12fps，但眨眼要比常规帧再快一档：83ms 一格会让
 * 半闭 → 闭 → 半闭拖成 0.33 秒，像犯困而不是眨眼。
 */
const BLINK_STEP_MS = 68;
/** 两次眨眼之间：2.4~7.4 秒随机，刻意不与任何循环对齐（对齐就是机械感的来源）。 */
const BLINK_GAP_MIN_MS = 2400;
const BLINK_GAP_SPAN_MS = 5000;
/** 连着眨第二下的概率与间隔：真人的眨眼里有相当一部分是双眨。 */
const DOUBLE_BLINK_CHANCE = 0.16;
const DOUBLE_BLINK_GAP_MS = 120;

interface Props {
  skin: PetSkinDef;
  mood: PetMood;
  phase: BubblePhase;
}

/**
 * 桌宠舞台：把情绪演出来。
 *
 * 素材由 `src/assets/pets/<皮肤>/` 自动进清单：一段带 alpha 的 WebM 直接循环播（角色自己
 * 会呼吸、会冒星星，情绪就写在动作里），多帧静图按 fps 轮播，缺某情绪退回 idle。
 * 非视频素材另有一层 FX（思考点、绕轨、拍玻璃、乌云、Zzz），视频自己带特效就不再叠。
 *
 * 分层（由外到内，动效互不打架）：
 *   .qa-pet        指针倾角（跟着鼠标微微转，像在看你）
 *   .qa-pet-body   交互形变：悬停抬起 / 按下压扁 / 拎起摆动 / 落地回弹
 *   .qa-pet-sway   常驻的待机摇摆（不规则周期，所以不像机器人）
 *   素材 / 机甲     情绪动画（呼吸、打字、敲玻璃…）
 */
export default function PetStage({ skin, mood, phase }: Props) {
  const gestureName = phase === "dragging" ? "drag" : phase === "firing" ? "tap" : null;
  const clip =
    (gestureName ? petGestureFor(skin, gestureName) : undefined) ?? petClipFor(skin, mood);
  const [index, setIndex] = useState(0);
  /** 眨眼序列播到第几格；-1 = 没在眨。 */
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

  /**
   * 眨眼（2026-10-25）：从情绪节拍里拆出来随机插播。
   *
   * 原来的眨眼钉在 idle 的节拍表里，而播放器是定频轮播 —— 每 5.5 秒的同一个拍子上
   * 眨同一双眼睛，看久了就是「机械」。现在 idle 只循环呼吸与歪头，眨眼由这里的随机
   * 计时器决定什么时候发生、要不要连着眨两下；只认完全待机的状态，干活/等审批的节奏
   * 仍然写在素材节拍表里。
   */
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
  /** 眨眼帧盖住基础帧：素材里同一张图上只有眼睛不同，盖上去就是"眨"。 */
  const frame = blinking ?? baseFrame;
  /** 素材目录是空的（自备包还没放帧）：宁可显示圆盘，也不要一扇看不见的窗 */
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
          {!clip?.video && <PetFx mood={mood} />}
        </>
      )}
    </div>
  );
}

/** 情绪特效层：只有 transform / opacity，窗口拖动时不重排。 */
function PetFx({ mood }: { mood: PetMood }) {
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
      <span className="qa-fx qa-fx-orbit" aria-hidden>
        <i />
      </span>
    );
  }
  if (mood === "waiting") {
    return (
      <span className="qa-fx qa-fx-knock" aria-hidden>
        ?
      </span>
    );
  }
  if (mood === "happy") {
    return (
      <>
        <span className="qa-fx qa-fx-spark" aria-hidden>
          <i />
          <i />
          <i />
        </span>
        {/* 被摸头 / 干完活：冒一颗爱心，比只有火花萌 */}
        <span className="qa-fx qa-fx-heart" aria-hidden>
          ♥
        </span>
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

/** Slim geometric spark — reads cleaner than Lucide Sparkles at small size. */
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
