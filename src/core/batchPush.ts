/**
 * One push, with the "rejected → ask → merge remote → push again" recovery.
 *
 * The commit page always had this loop; the repos page used to fail straight
 * into its batch report. Both pages now run every push through here so the
 * entry point does not change what a push does. The confirm dialog and the
 * merge pull are injected: the commit page keeps its conflict/MergeResolver
 * bookkeeping, the repos page just lets the error land in its report.
 */
import type { TFunction } from "i18next";
import { parseInvokeError } from "../hooks/useInvokeError";

export type PushRecoveryDeps = {
  push: (path: string) => Promise<unknown>;
  /** `git pull --no-rebase` — the merge side of the recovery. */
  mergePull: (path: string) => Promise<unknown>;
  confirm: (options: {
    title: string;
    message: string;
    confirmText?: string;
    icon?: "danger" | "warning" | "info" | "question";
  }) => Promise<boolean>;
  t: TFunction<"git">;
  label: (path: string) => string;
};

/** A rejected push is worth the recovery question; a network drop is not. */
export function isPushRejected(err: unknown): boolean {
  return parseInvokeError(err).code === "GIT_PUSH_REJECTED";
}

export async function pushOneWithRecovery(
  deps: PushRecoveryDeps,
  repoPath: string
): Promise<void> {
  try {
    await deps.push(repoPath);
  } catch (e) {
    if (!isPushRejected(e)) throw e;
    const ok = await deps.confirm({
      title: deps.t("commit.pushRejectedTitle"),
      message: deps.t("commit.pushRejectedMessage", { name: deps.label(repoPath) }),
      confirmText: deps.t("commit.pushRejectedMerge"),
      icon: "warning",
    });
    if (!ok) throw e;
    await deps.mergePull(repoPath);
    await deps.push(repoPath);
  }
}
