import { useEffect, useState } from "react";
import { petClipFor, petGestureFor, type PetMood, type PetSkinDef } from "../../lib/petSkins";

/** 交互阶段（按下/拖拽/发射）与情绪是两条独立轴，CSS 各管一层。 */
export type BubblePhase = "idle" | "pressed" | "dragging" | "firing";

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
 * 矢量素材另有一层 FX（思考点、绕轨、拍玻璃、乌云、Zzz）；视频素材自己带特效，就不再叠一层。
 */
export default function PetStage({ skin, mood, phase }: Props) {
  const gestureName = phase === "dragging" ? "drag" : phase === "firing" ? "tap" : null;
  const clip =
    (gestureName ? petGestureFor(skin, gestureName) : undefined) ?? petClipFor(skin, mood);
  const [index, setIndex] = useState(0);

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

  const frame = clip?.frames.length ? clip.frames[index % clip.frames.length] : undefined;
  /** 素材目录是空的（自备包还没放帧）：宁可显示圆盘，也不要一扇看不见的窗 */
  const bare = skin.kind !== "orb" && !frame;

  return (
    <div className={`qa-pet mood-${mood} is-${phase}`} data-skin={skin.id}>
      {skin.kind === "orb" || bare ? (
        <div className="qa-bubble-orb">
          <BubbleMark className="qa-bubble-icon" />
        </div>
      ) : (
        <>
          <div className="qa-pet-body">
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
      <span className="qa-fx qa-fx-spark" aria-hidden>
        <i />
        <i />
        <i />
      </span>
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
