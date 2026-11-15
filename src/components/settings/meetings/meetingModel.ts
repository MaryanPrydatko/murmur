import type { TFunction } from "i18next";
import type {
  MeetingRow,
  MeetingSegment,
  MeetingSpeaker,
  MeetingStep,
} from "@/bindings";

/** What `meetings.status` holds; anything else reads as done. */
export type MeetingStatus = "recording" | "transcribing" | "done" | "failed";

export const meetingStatus = (row: MeetingRow): MeetingStatus =>
  row.status === "recording" ||
  row.status === "transcribing" ||
  row.status === "failed"
    ? row.status
    : "done";

/** The transcription steps, in the order `pipeline.rs` runs them. */
export const MEETING_STEPS: MeetingStep[] = ["mic", "system", "speakers"];

/**
 * Where playing from `at` starts, in meeting seconds: never before the first
 * track has sound, so the clock doesn't sit still through the silence.
 */
export const playbackStart = (at: number, offsets: number[]) =>
  Math.max(at, Math.min(...offsets));

/** The speaker's colour slot: `me`, `s1`..`s4`; `them` shares `s1`'s. */
export const speakerSlot = (key: string): string => {
  if (key === "me" || key === "them") return key;
  const n = Number(key.slice(1));
  return Number.isInteger(n) && n > 0 ? `s${((n - 1) % 4) + 1}` : "s1";
};

/** A name shown even when the user gave none. */
export const speakerName = (
  t: TFunction,
  key: string,
  label?: string | null,
): string => {
  if (label) return label;
  if (key === "me") return t("meetings.me");
  if (key === "them") return t("meetings.others");
  return t("meetings.speaker", { n: key.replace(/^s/, "") });
};

/** `s1`, `s2` without a name get a dashed chip; Me and Others are named. */
export const isUnnamed = (speaker: MeetingSpeaker) =>
  !speaker.label && speaker.key !== "me" && speaker.key !== "them";

/** `me` first, then `them`, then `s1`, `s2`, ... */
export const speakerOrder = (a: string, b: string) => {
  const rank = (k: string) =>
    k === "me" ? -2 : k === "them" ? -1 : Number(k.slice(1)) || 0;
  return rank(a) - rank(b);
};

export const startedAt = (row: MeetingRow): Date | null =>
  row.started_at == null ? null : new Date(row.started_at * 1000);

const timeFormat = (lang: string) =>
  new Intl.DateTimeFormat(lang, {
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });

export const formatTime = (lang: string, date: Date) =>
  timeFormat(lang).format(date);

/** "Meeting 15 Nov, 15:10" when the meeting has no title. */
export const meetingTitle = (t: TFunction, lang: string, row: MeetingRow) => {
  if (row.title) return row.title;
  const date = startedAt(row);
  if (!date) return t("meetings.untitled");
  const day = new Intl.DateTimeFormat(lang, {
    day: "numeric",
    month: "short",
  }).format(date);
  return t("meetings.defaultTitle", {
    date: `${day}, ${formatTime(lang, date)}`,
  });
};

/** "Sun 15 Nov 2026, 14:03". */
export const meetingDate = (lang: string, row: MeetingRow) => {
  const date = startedAt(row);
  if (!date) return "";
  const day = new Intl.DateTimeFormat(lang, {
    weekday: "short",
    day: "numeric",
    month: "short",
    year: "numeric",
  }).format(date);
  return `${day}, ${formatTime(lang, date)}`;
};

/** 47 min, 1 h 13 min. */
export const formatDuration = (t: TFunction, ms: number | null) => {
  if (ms == null) return "";
  const minutes = Math.max(1, Math.round(ms / 60000));
  if (minutes < 60) return t("meetings.minutes", { count: minutes });
  return t("meetings.hoursMinutes", {
    hours: Math.floor(minutes / 60),
    minutes: String(minutes % 60).padStart(2, "0"),
  });
};

/** 00:41, 1:02:05. */
export const formatClock = (seconds: number) => {
  const s = Math.max(0, Math.floor(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const mm = h
    ? `${h}:${String(m).padStart(2, "0")}`
    : String(m).padStart(2, "0");
  return `${mm}:${String(s % 60).padStart(2, "0")}`;
};

export const languageName = (lang: string, code: string | null) => {
  if (!code) return "";
  try {
    return new Intl.DisplayNames([lang], { type: "language" }).of(code) ?? code;
  } catch {
    return code;
  }
};

export type DayGroup =
  | { kind: "today" | "yesterday" | "thisWeek" | "lastWeek" | "undated" }
  | { kind: "month"; label: string };

const dayStart = (d: Date) =>
  new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();

/** Which heading a meeting sits under in the list, from `now`. */
export const dayGroup = (
  lang: string,
  row: MeetingRow,
  now: Date,
): DayGroup => {
  const date = startedAt(row);
  if (!date) return { kind: "undated" };
  const days = Math.round((dayStart(now) - dayStart(date)) / 86_400_000);
  if (days <= 0) return { kind: "today" };
  if (days === 1) return { kind: "yesterday" };
  // Weeks start on Monday.
  const weekday = (now.getDay() + 6) % 7;
  if (days <= weekday) return { kind: "thisWeek" };
  if (days <= weekday + 7) return { kind: "lastWeek" };
  return {
    kind: "month",
    label: new Intl.DateTimeFormat(lang, {
      month: "long",
      year: "numeric",
    }).format(date),
  };
};

export const dayGroupKey = (g: DayGroup) =>
  g.kind === "month" ? `month:${g.label}` : g.kind;

/** Each speaker's share of the talking, in whole percent. */
export const talkShares = (segments: MeetingSegment[]) => {
  const totals = new Map<string, number>();
  for (const s of segments) {
    totals.set(
      s.speaker_key,
      (totals.get(s.speaker_key) ?? 0) + Math.max(0, s.t1_ms - s.t0_ms),
    );
  }
  const all = [...totals.values()].reduce((a, b) => a + b, 0);
  const shares = new Map<string, number>();
  for (const [key, ms] of totals) {
    shares.set(key, all ? Math.round((ms / all) * 100) : 0);
  }
  return shares;
};

/** Consecutive segments of one speaker, read as one turn. */
export interface Turn {
  speaker: string;
  t0_ms: number;
  /** Indexes into the meeting's segments. */
  segments: number[];
}

export const toTurns = (segments: MeetingSegment[]): Turn[] => {
  const turns: Turn[] = [];
  segments.forEach((s, i) => {
    const last = turns[turns.length - 1];
    if (last && last.speaker === s.speaker_key) last.segments.push(i);
    else turns.push({ speaker: s.speaker_key, t0_ms: s.t0_ms, segments: [i] });
  });
  return turns;
};

/** Segment shown as playing at meeting time `ms`, or -1. */
export const segmentAt = (segments: MeetingSegment[], ms: number) =>
  segments.findIndex((s) => ms >= s.t0_ms && ms < s.t1_ms + 300);

/** Where `query` occurs in `text`, case-insensitively, as [start, end). */
export const matchRanges = (
  text: string,
  query: string,
): [number, number][] => {
  const q = query.trim().toLocaleLowerCase();
  if (!q) return [];
  const hay = text.toLocaleLowerCase();
  const out: [number, number][] = [];
  for (let i = hay.indexOf(q); i >= 0; i = hay.indexOf(q, i + q.length)) {
    out.push([i, i + q.length]);
  }
  return out;
};
