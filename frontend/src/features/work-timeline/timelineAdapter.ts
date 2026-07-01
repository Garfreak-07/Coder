import type { TimelineItem } from "./timelineTypes";

export function timelineItemTitle(item: TimelineItem): string {
  switch (item.type) {
    case "user_message":
      return "User request";
    case "planner_message":
      return "Planner message";
    case "reasoning_summary":
      return item.agent_id === "planner" ? "Planner reasoning" : "Executor reasoning";
    case "plan_update":
      return item.title;
    case "executor_step":
      return item.title;
    case "tool_call":
      return `Tool ${timelineItemStatus(item).toLowerCase()}: ${item.tool_name}`;
    case "command_execution":
      return "Command execution";
    case "file_change":
      return "File change";
    case "approval":
      return "Approval";
    case "verification":
      return "Verification";
    case "final_summary":
      return "Final summary";
    default:
      return "Timeline item";
  }
}

export function timelineItemStatus(item: TimelineItem): string {
  if ("status" in item && typeof item.status === "string") return humanStatus(item.status);
  if (item.type === "file_change") return humanStatus(item.change_type);
  return item.type.replaceAll("_", " ");
}

export function timelineItemTone(item: TimelineItem): "neutral" | "success" | "warning" | "danger" {
  const status = "status" in item && typeof item.status === "string" ? item.status : "";
  const changeType = item.type === "file_change" ? item.change_type : "";
  const value = `${status} ${changeType}`.toLowerCase();

  if (value.includes("fail") || value.includes("error") || value.includes("conflict")) {
    return "danger";
  }
  if (value.includes("block") || value.includes("cancel") || value.includes("approval")) {
    return "warning";
  }
  if (
    value.includes("complete") ||
    value.includes("success") ||
    value.includes("applied") ||
    value.includes("modified") ||
    value.includes("added")
  ) {
    return "success";
  }
  return "neutral";
}

export function commandLine(command: unknown): string {
  const parts = Array.isArray(command) ? command.filter((part): part is string => typeof part === "string") : [];
  return parts.length > 0 ? parts.join(" ") : "command";
}

export function formatDuration(durationMs?: number | null): string | null {
  if (typeof durationMs !== "number") return null;
  if (durationMs < 1000) return `${durationMs} ms`;
  return `${(durationMs / 1000).toFixed(1)} s`;
}

export function humanStatus(status: string): string {
  const normalized = status.toLowerCase().replaceAll("_", " ");
  switch (normalized) {
    case "completed":
    case "complete":
    case "success":
    case "succeeded":
    case "applied":
      return "Completed";
    case "started":
    case "running":
      return "Running";
    case "selected":
      return "Selected";
    case "previewed":
    case "pending":
      return "Pending";
    case "blocked":
      return "Blocked";
    case "failed":
    case "failure":
    case "error":
      return "Failed";
    case "cancelled":
    case "canceled":
      return "Cancelled";
    case "modified":
      return "Modified";
    case "added":
      return "Added";
    case "deleted":
      return "Deleted";
    default:
      return normalized.length > 0 ? normalized[0].toUpperCase() + normalized.slice(1) : "Update";
  }
}
