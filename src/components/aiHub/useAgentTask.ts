import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AiHubPlanStep } from "../../core/types";

export interface AgentTask {
  sessionId: string;
  taskId: string;
  requestId: string;
  goal: string;
  workspace: string;
  branch: string | null;
  status: "running" | "waiting_approval" | "ended" | "interrupted";
  reason: string | null;
  needsReview: boolean;
  plan: AiHubPlanStep[];
  actions: {
    id: number;
    name: string;
    summary: string;
    paths: string[];
    status: "pending" | "uncertain" | "done" | "failed" | "reviewed";
    exitCode: number | null;
    detail: string | null;
  }[];
  verification: {
    status: "unverified" | "passed" | "failed" | "stale";
    command: string | null;
    exitCode: number | null;
  };
}

/** Read the durable state on session open and apply live transitions for that session. */
export function useAgentTask(sessionId: string | null): AgentTask | null {
  const [task, setTask] = useState<AgentTask | null>(null);
  const generation = useRef(0);
  useEffect(() => {
    if (!sessionId) return;
    let closed = false;
    let refresh = () => {};
    const subscription = listen<AgentTask>("agent-task-state", ({ payload }) => {
      if (closed || payload.sessionId !== sessionId) return;
      generation.current++;
      setTask(payload);
    });
    void subscription.then(() => {
      if (closed) return;
      refresh = () => {
        const request = ++generation.current;
        void invoke<AgentTask | null>("agent_task_load", { sessionId }).then((snapshot) => {
          if (!closed && request === generation.current) setTask(snapshot);
        }).catch(() => {});
      };
      refresh();
    }).catch(() => {});
    const onFocus = () => refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      closed = true;
      window.removeEventListener("focus", onFocus);
      void subscription.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [sessionId]);
  return sessionId && task?.sessionId === sessionId ? task : null;
}
