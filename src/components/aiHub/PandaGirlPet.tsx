import { useEffect, useRef, useState } from "react";
import type { PetMood } from "../../lib/petSkins";
import type { BubblePhase } from "./PetStage";
import idleUrl from "../../assets/pets/panda-girl/idle.png";
import sleepyUrl from "../../assets/pets/panda-girl/sleepy.png";
import workingUrl from "../../assets/pets/panda-girl/working.png";
import waitingUrl from "../../assets/pets/panda-girl/waiting.png";
import errorUrl from "../../assets/pets/panda-girl/error.png";
import thinkingUrl from "../../assets/pets/panda-girl/thinking.png";
import dragUrl from "../../assets/pets/panda-girl/drag.png";

/** 立绘迭代时 bump，逼 WebView 丢掉旧 PNG 缓存。 */
const ART_REV = "q6";
const rev = (url: string) => `${url}${url.includes("?") ? "&" : "?"}r=${ART_REV}`;

const IDLE = rev(idleUrl);
const SLEEPY = rev(sleepyUrl);
const WORKING = rev(workingUrl);
const WAITING = rev(waitingUrl);
const ERROR = rev(errorUrl);
const THINKING = rev(thinkingUrl);
/** 被拎起：张手悬脚。与 idle 同底同比例，换图不会跳。 */
const DRAG = rev(dragUrl);

/** agent 长驻情绪才换图；happy/idle 一律 idle，避免比例跳。 */
const STICKY_ART: Partial<Record<PetMood, string>> = {
  thinking: THINKING,
  working: WORKING,
  waiting: WAITING,
  error: ERROR,
  sleepy: SLEEPY,
};

export type PetMicro =
  | "none"
  | "ear"
  | "stretch"
  | "glance"
  | "wave"
  | "peek"
  | "bounce"
  | "shy"
  | "nod";

export type PressZone = "head" | "body" | "tail";

interface Props {
  mood: PetMood;
  phase: BubblePhase;
  bondTier: 0 | 1 | 2;
  gesture?: "pat" | "poke" | "tail" | "hug" | "shy" | null;
  micro?: PetMicro;
  celebrate?: boolean;
  pressZone?: PressZone | null;
}

function pickArt(mood: PetMood, phase: BubblePhase): string {
  if (phase === "dragging") return DRAG;
  return STICKY_ART[mood] ?? IDLE;
}

/**
 * 原创 Q 版熊猫娘：同底 idle + CSS 表情/动作。
 * 眨眼改为眼线处轻压扁（不叠眼皮、不换 blink 图，避免对不齐）。
 */
export default function PandaGirlPet({
  mood,
  phase,
  bondTier,
  gesture = null,
  micro = "none",
  celebrate = false,
  pressZone = null,
}: Props) {
  const [blink, setBlink] = useState(false);
  const [front, setFront] = useState(() => pickArt(mood, phase));
  const [back, setBack] = useState<string | null>(null);
  const [fading, setFading] = useState(false);
  const [fadeOn, setFadeOn] = useState(false);
  const frontRef = useRef(front);
  frontRef.current = front;

  useEffect(() => {
    if (mood !== "idle" || phase !== "idle" || gesture || micro !== "none") {
      setBlink(false);
      return;
    }
    let timer = 0;
    let cancelled = false;
    const schedule = () => {
      timer = window.setTimeout(() => {
        if (cancelled) return;
        setBlink(true);
        timer = window.setTimeout(() => {
          if (cancelled) return;
          setBlink(false);
          if (Math.random() < 0.16) {
            timer = window.setTimeout(() => {
              if (cancelled) return;
              setBlink(true);
              timer = window.setTimeout(() => {
                if (!cancelled) {
                  setBlink(false);
                  schedule();
                }
              }, 55);
            }, 80);
          } else {
            schedule();
          }
        }, 90);
      }, 2800 + Math.random() * 5000);
    };
    schedule();
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [mood, phase, gesture, micro]);

  const want = pickArt(mood, phase);

  useEffect(() => {
    const cur = frontRef.current;
    if (want === cur) return;
    setBack(cur);
    setFront(want);
    setFading(true);
    setFadeOn(false);
    let raf = 0;
    let t = 0;
    raf = window.requestAnimationFrame(() => {
      raf = window.requestAnimationFrame(() => setFadeOn(true));
    });
    t = window.setTimeout(() => {
      setBack(null);
      setFading(false);
      setFadeOn(false);
    }, 280);
    return () => {
      window.cancelAnimationFrame(raf);
      window.clearTimeout(t);
    };
  }, [want]);

  useEffect(() => {
    for (const url of Object.values(STICKY_ART)) {
      if (!url) continue;
      const img = new Image();
      img.src = url;
    }
    for (const url of [IDLE, DRAG]) {
      const img = new Image();
      img.src = url;
    }
  }, []);

  const g = gesture ?? "none";
  const expressHappy =
    mood === "happy" || g === "pat" || g === "hug" || g === "tail" || celebrate;
  const expressShy = g === "shy" || micro === "shy";

  const className = [
    "qa-panda-girl",
    `mood-${mood}`,
    `is-${phase}`,
    `bond-${bondTier}`,
    g !== "none" ? `gest-${g}` : "",
    micro !== "none" ? `micro-${micro}` : "",
    celebrate ? "is-celebrate" : "",
    blink ? "is-blinking" : "",
    fading ? "is-fading" : "",
    fading && fadeOn ? "is-fade-on" : "",
    expressHappy ? "expr-happy" : "",
    expressShy ? "expr-shy" : "",
    phase === "pressed" && pressZone ? `press-${pressZone}` : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={className} aria-hidden>
      <span className="qa-panda-girl-glow" />
      <span className="qa-panda-girl-shadow" />
      <div className="qa-panda-girl-body-wrap">
        <div className="qa-panda-girl-stage">
          {back && <img className="qa-panda-girl-art is-back" src={back} alt="" draggable={false} />}
          <div className="qa-panda-girl-gaze">
            <img className="qa-panda-girl-art is-front" src={front} alt="" draggable={false} />
            <span className="qa-panda-girl-blush" />
            <span className="qa-panda-girl-specular" />
          </div>
        </div>
      </div>
    </div>
  );
}
