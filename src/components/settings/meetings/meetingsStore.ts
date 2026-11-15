import { useEffect } from "react";
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { commands, type MeetingRow } from "@/bindings";
import { meetingStatus, type MeetingProgress } from "./meetingModel";

/** How often the list is read again while a meeting records or transcribes. */
const BUSY_POLL_MS = 2000;

interface MeetingsStore {
  /** Newest first; null until the first read. */
  rows: MeetingRow[] | null;
  /** Speaker keys per meeting id, for the dots in the list. */
  speakers: Record<string, string[]>;
  error: string | null;
  /** The meeting that's recording, if one is. */
  recordingId: string | null;
  progress: Record<string, MeetingProgress>;
  refresh: () => Promise<void>;
  toggle: () => Promise<void>;
}

export const useMeetingsStore = create<MeetingsStore>((set, get) => ({
  rows: null,
  speakers: {},
  error: null,
  recordingId: null,
  progress: {},
  refresh: async () => {
    try {
      const [rows, speakers, recording] = await Promise.all([
        commands.listMeetings(),
        commands.listMeetingSpeakers(),
        commands.isMeetingRecording(),
      ]);
      if (rows.status === "error") {
        set({ error: rows.error });
        return;
      }
      // A `recording` row left by a crash isn't recording; only the newest
      // one is, and only while the backend says so.
      const live = recording
        ? rows.data.find((r) => meetingStatus(r) === "recording")
        : undefined;
      set({
        rows: rows.data,
        error: null,
        recordingId: live?.id ?? null,
        speakers:
          speakers.status === "ok"
            ? Object.fromEntries(
                speakers.data.map((s) => [s.meeting_id, s.keys]),
              )
            : get().speakers,
      });
    } catch (e) {
      set({ error: String(e) });
    }
  },
  toggle: async () => {
    await commands.toggleMeeting();
    // The backend opens devices on its own thread; read again once it has.
    setTimeout(() => get().refresh(), 400);
  },
}));

const isBusy = (rows: MeetingRow[] | null, recordingId: string | null) =>
  recordingId !== null ||
  (rows ?? []).some((r) => meetingStatus(r) === "transcribing");

/**
 * Keeps the meetings store current: reads it once, again on
 * `meetings-changed` and `meeting-progress`, and every couple of seconds
 * while something is recording or transcribing. Mount once, in App.
 */
export const useMeetingsSync = () => {
  const refresh = useMeetingsStore((s) => s.refresh);
  const busy = useMeetingsStore((s) => isBusy(s.rows, s.recordingId));

  useEffect(() => {
    refresh();
    const changed = listen("meetings-changed", () => refresh());
    const progress = listen<MeetingProgress>("meeting-progress", (event) => {
      const p = event.payload;
      useMeetingsStore.setState((s) => ({
        progress: { ...s.progress, [p.meeting_id]: p },
      }));
    });
    return () => {
      changed.then((off) => off());
      progress.then((off) => off());
    };
  }, [refresh]);

  useEffect(() => {
    if (!busy) return;
    const id = setInterval(refresh, BUSY_POLL_MS);
    return () => clearInterval(id);
  }, [busy, refresh]);
};
