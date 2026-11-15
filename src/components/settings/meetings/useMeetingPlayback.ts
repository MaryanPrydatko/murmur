import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { readFile } from "@tauri-apps/plugin-fs";
import { commands, type MeetingAudio } from "@/bindings";
import { useOsType } from "@/hooks/useOsType";

/** Which recorded side to hear. */
export type TrackChoice = "both" | "me" | "others";
export const SPEEDS = [1, 1.25, 1.5, 2] as const;

/** Tracks further apart than this are pulled back together. */
const MAX_DRIFT_S = 0.15;

interface LoadedTrack {
  audio: HTMLAudioElement;
  kind: string;
  offset: number;
  blobUrl: string | null;
}

const audible = (kind: string, choice: TrackChoice) =>
  choice === "both" ||
  kind === "mixed" ||
  (choice === "me" ? kind === "mic" : kind === "system");

/**
 * Plays a meeting's tracks in step, on the meeting's own clock: segment times
 * and `t` are seconds from the meeting's start, and each track starts at its
 * own offset. Loads the audio on the first play.
 */
export const useMeetingPlayback = (meetingId: string, durationS: number) => {
  const osType = useOsType();
  const [t, setT] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [loading, setLoading] = useState(false);
  const [failed, setFailed] = useState(false);
  const [speed, setSpeed] = useState<(typeof SPEEDS)[number]>(1);
  const [choice, setChoice] = useState<TrackChoice>("both");
  const [kinds, setKinds] = useState<string[]>([]);
  const tracks = useRef<LoadedTrack[] | null>(null);
  const tRef = useRef(0);
  const waits = useRef<ReturnType<typeof setTimeout>[]>([]);

  useEffect(() => {
    let stale = false;
    commands.meetingAudio(meetingId).then((r) => {
      if (!stale && r.status === "ok") setKinds(r.data.map((a) => a.kind));
    });
    return () => {
      stale = true;
      waits.current.forEach(clearTimeout);
      for (const track of tracks.current ?? []) {
        track.audio.pause();
        if (track.blobUrl) URL.revokeObjectURL(track.blobUrl);
      }
      tracks.current = null;
      tRef.current = 0;
      setT(0);
      setPlaying(false);
      setFailed(false);
    };
  }, [meetingId]);

  const source = useCallback(
    async (track: MeetingAudio) => {
      // WebKitGTK doesn't play the asset protocol; History reads it whole.
      if (osType === "linux") {
        const data = await readFile(track.path);
        const url = URL.createObjectURL(
          new Blob([data], { type: "audio/wav" }),
        );
        return { src: url, blobUrl: url };
      }
      return { src: convertFileSrc(track.path, "asset"), blobUrl: null };
    },
    [osType],
  );

  const load = useCallback(async () => {
    if (tracks.current) return tracks.current;
    const result = await commands.meetingAudio(meetingId);
    if (result.status !== "ok" || result.data.length === 0) {
      throw new Error(result.status === "ok" ? "no tracks" : result.error);
    }
    const loaded: LoadedTrack[] = [];
    for (const track of result.data) {
      const { src, blobUrl } = await source(track);
      const audio = new Audio(src);
      audio.preload = "auto";
      loaded.push({
        audio,
        kind: track.kind,
        offset: track.start_offset_ms / 1000,
        blobUrl,
      });
    }
    tracks.current = loaded;
    return loaded;
  }, [meetingId, source]);

  const place = useCallback((list: LoadedTrack[], at: number) => {
    waits.current.forEach(clearTimeout);
    waits.current = [];
    for (const track of list) {
      track.audio.currentTime = Math.max(0, at - track.offset);
    }
  }, []);

  const startAll = useCallback(
    (list: LoadedTrack[], at: number) => {
      place(list, at);
      for (const track of list) {
        const wait = track.offset - at;
        if (wait > 0) {
          waits.current.push(
            setTimeout(() => track.audio.play().catch(() => {}), wait * 1000),
          );
        } else {
          track.audio.play().catch(() => {});
        }
      }
    },
    [place],
  );

  const pause = useCallback(() => {
    waits.current.forEach(clearTimeout);
    for (const track of tracks.current ?? []) track.audio.pause();
    setPlaying(false);
  }, []);

  const play = useCallback(
    async (from?: number) => {
      const at = Math.min(durationS, Math.max(0, from ?? tRef.current));
      tRef.current = at;
      setT(at);
      try {
        setLoading(!tracks.current);
        const list = await load();
        startAll(list, at >= durationS ? 0 : at);
        setPlaying(true);
        setFailed(false);
      } catch (e) {
        console.error("Failed to play the meeting:", e);
        setFailed(true);
      } finally {
        setLoading(false);
      }
    },
    [durationS, load, startAll],
  );

  const seek = useCallback(
    (to: number) => {
      const at = Math.min(durationS, Math.max(0, to));
      tRef.current = at;
      setT(at);
      const list = tracks.current;
      if (!list) return;
      if (playing) startAll(list, at);
      else place(list, at);
    },
    [durationS, place, playing, startAll],
  );

  // The meeting clock follows the first track that's playing; the others
  // are pulled back when they drift.
  useEffect(() => {
    if (!playing) return;
    let frame = 0;
    const tick = () => {
      const list = tracks.current ?? [];
      const lead = list.find((x) => !x.audio.paused && !x.audio.ended);
      if (lead) {
        const now = lead.audio.currentTime + lead.offset;
        for (const other of list) {
          if (other === lead || other.audio.paused) continue;
          const drift = other.audio.currentTime + other.offset - now;
          if (Math.abs(drift) > MAX_DRIFT_S) {
            other.audio.currentTime = Math.max(0, now - other.offset);
          }
        }
        tRef.current = now;
        setT(now);
      } else if (list.every((x) => x.audio.ended)) {
        setPlaying(false);
        return;
      }
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [playing]);

  useEffect(() => {
    for (const track of tracks.current ?? []) {
      track.audio.playbackRate = speed;
      track.audio.muted = !audible(track.kind, choice);
    }
  }, [speed, choice, playing]);

  return {
    t,
    playing,
    loading,
    failed,
    speed,
    choice,
    /** Track choice only means something with a mic and a system track. */
    canChoose: kinds.includes("mic") && kinds.includes("system"),
    play,
    pause,
    seek,
    cycleSpeed: () =>
      setSpeed((s) => SPEEDS[(SPEEDS.indexOf(s) + 1) % SPEEDS.length]),
    cycleChoice: () =>
      setChoice((c) => (c === "both" ? "me" : c === "me" ? "others" : "both")),
  };
};

export type MeetingPlayback = ReturnType<typeof useMeetingPlayback>;
