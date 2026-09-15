export type FloatingMenuStyle = {
  top?: number;
  bottom?: number;
  left: number;
  width: number;
  maxHeight: number;
};

/**
 * Position a dropdown under (or above) its trigger.
 *
 * Menus render into `document.body` with `position: fixed`, so a scroll container (the
 * prompts toolbar, the models modal body) cannot clip them. Passing `bottom` instead of
 * `top` flips the menu upwards when there is more room above.
 */
export function computeFloatingMenuStyle(
  trigger: HTMLElement,
  opts: { align: "left" | "right"; minWidth: number; preferMaxHeight: number }
): FloatingMenuStyle {
  const rect = trigger.getBoundingClientRect();
  const gap = 4;
  const pad = 8;
  const vh = window.innerHeight;
  const vw = window.innerWidth;
  const width = Math.min(Math.max(opts.minWidth, rect.width), vw - pad * 2);
  const spaceBelow = vh - rect.bottom - gap - pad;
  const spaceAbove = rect.top - gap - pad;
  const openUp = spaceBelow < 200 && spaceAbove > spaceBelow;
  const maxHeight = Math.min(
    opts.preferMaxHeight,
    Math.max(140, openUp ? spaceAbove : spaceBelow)
  );
  let left = opts.align === "right" ? rect.right - width : rect.left;
  left = Math.min(Math.max(pad, left), vw - width - pad);
  if (openUp) {
    return { bottom: vh - rect.top + gap, left, width, maxHeight };
  }
  return { top: rect.bottom + gap, left, width, maxHeight };
}
