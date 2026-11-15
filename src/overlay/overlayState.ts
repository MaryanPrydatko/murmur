/** What the backend asks the overlay to show (`show-overlay`). */
export type Session =
  | "recording"
  | "streaming"
  | "transcribing"
  | "processing"
  | "notice"
  | "live_notice";

/** The island is the Mac notch; the pill is everywhere else. */
export type OverlayForm = "island" | "pill";

/** Sent as `overlay-form` before the overlay shows. */
export interface OverlayGeometry {
  form: OverlayForm;
  /** The notch, in points; only set for the island. */
  notch_width: number | null;
  notch_height: number | null;
}

/** Each state the mock draws, by the name its CSS uses. */
export type VisualState =
  | "idle"
  | "arming"
  | "listening"
  | "transcribing"
  | "processing"
  | "live"
  | "live-working"
  | "meeting"
  | "meeting-dictating"
  | "meeting-controls"
  | "meeting-transcribing"
  | "notice-warn"
  | "notice-error"
  | "notice-info";

export interface OverlayInputs {
  /** null once the session hides. */
  session: Session | null;
  /** The microphone has delivered audio. */
  captureReady: boolean;
  /** Live: finishing the last words. */
  working: boolean;
  /** Live: something to show in the text area. */
  hasText: boolean;
  noticeLevel: "error" | "warning" | "info" | null;
  /** A meeting is recording. */
  meeting: boolean;
  /** The island's meeting controls are open. */
  controls: boolean;
  /** A finished meeting is transcribing and the overlay shows it. */
  meetingWork: boolean;
}

const NOTICE: Record<string, VisualState> = {
  error: "notice-error",
  warning: "notice-warn",
  info: "notice-info",
};

const listening = (i: OverlayInputs): VisualState => {
  if (i.meeting) return "meeting-dictating";
  return i.captureReady ? "listening" : "arming";
};

export const visualState = (i: OverlayInputs): VisualState => {
  switch (i.session) {
    case "notice":
    case "live_notice":
      return NOTICE[i.noticeLevel ?? "error"];
    case "transcribing":
    case "processing":
      return i.session;
    case "streaming":
      if (i.working) return i.hasText ? "live-working" : "transcribing";
      if (i.hasText) return "live";
      // A Live session with no text yet looks like a recording.
      return listening(i);
    case "recording":
      return listening(i);
    case null:
      if (i.meeting) return i.controls ? "meeting-controls" : "meeting";
      return i.meetingWork ? "meeting-transcribing" : "idle";
  }
};

/**
 * Heights for `n` bars from the 16 `mic-level` buckets. Bar k reads the
 * bucket equal to its rank outward from the centre, tapered with distance,
 * so the lowest frequencies sit in the middle: a lens, not a slope.
 */
export const barHeights = (levels: number[], n: number, max: number) => {
  const mid = (n - 1) / 2;
  const rank = [...Array(n).keys()].sort(
    (a, b) => Math.abs(a - mid) - Math.abs(b - mid) || a - b,
  );
  const out = new Array<number>(n);
  rank.forEach((bar, bucket) => {
    const taper = 1 - 0.4 * (Math.abs(bar - mid) / (mid || 1));
    const v = Math.pow(levels[Math.min(bucket, 15)] ?? 0, 0.7) * taper;
    out[bar] = Math.max(3, Math.min(max, 3 + v * (max - 3) * 1.35));
  });
  return out;
};

/** 12:04, 1:02:05. */
export const formatTimer = (seconds: number) => {
  const s = Math.max(0, Math.floor(seconds));
  const head =
    s >= 3600
      ? `${Math.floor(s / 3600)}:${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}`
      : `${Math.floor(s / 60)}`;
  return `${head}:${String(s % 60).padStart(2, "0")}`;
};

/**
 * The whole pixels a measured rectangle touches: the part of the overlay
 * window that takes clicks.
 */
export const hitRect = (r: {
  left: number;
  top: number;
  right: number;
  bottom: number;
}) => {
  const x = Math.floor(r.left);
  const y = Math.floor(r.top);
  return {
    x,
    y,
    width: Math.ceil(r.right) - x,
    height: Math.ceil(r.bottom) - y,
  };
};
