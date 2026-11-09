import type { ReactNode } from "react";

type OrbitalLoadingVariant = "starting" | "stopping";

interface OrbitalLoadingStageProps {
  variant?: OrbitalLoadingVariant;
  icon: ReactNode;
  title: string;
  status: string;
  hint: string;
  className?: string;
}

/**
 * Shared launch / wait / close stage used by 协同办公 and Harness so the
 * orbital hero, status pill, and progress bar stay visually identical.
 */
export function OrbitalLoadingStage({
  variant = "starting",
  icon,
  title,
  status,
  hint,
  className,
}: OrbitalLoadingStageProps) {
  const classes = [
    "dapps-hint",
    variant === "stopping" ? "is-stopping" : "is-starting",
    className,
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={classes} role="status" aria-busy="true">
      <div className="dapps-start-stage" aria-hidden="true">
        <span className="dapps-start-aurora" />
        <span className="dapps-start-aurora is-alt" />
        <span className="dapps-start-grid" />
      </div>
      <div className="dapps-start-hero" aria-hidden="true">
        <span className="dapps-start-glow" />
        <span className="dapps-start-ring is-halo" />
        <span className="dapps-start-ring is-dashed" />
        <div className="dapps-start-orbit">
          <span className="dapps-start-orbit-track" />
          <span className="dapps-start-orbit-arc" />
          <span className="dapps-start-orbit-arc is-secondary" />
          <span className="dapps-start-spark" />
          <span className="dapps-start-spark is-lag" />
          <div className="dapps-start-icon">
            <span className="dapps-start-sheen" />
            {icon}
          </div>
        </div>
      </div>
      <div className="dapps-start-copy">
        <p className="dapps-hint-title">{title}</p>
        <div className="dapps-start-status">
          <span>{status}</span>
          <span className="dapps-start-dots">
            <i />
            <i />
            <i />
          </span>
        </div>
        <div className="dapps-start-bar">
          <span />
        </div>
        <p className="dapps-hint-text">{hint}</p>
      </div>
    </div>
  );
}
