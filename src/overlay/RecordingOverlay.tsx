import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import "./RecordingOverlay.css";
import { commands, events } from "@/bindings";
import { PATHS as TRAY_PATHS } from "@/components/icons/TrayIcon";
import type { Notice, StreamTextEvent, StreamWorkKind } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { noticeDisplay } from "@/lib/notices";
import type { ModelStateEvent } from "@/lib/types/events";
import { getLanguageDirection } from "@/lib/utils/rtl";
import {
  barHeights,
  formatTimer,
  visualState,
  type OverlayGeometry,
  type Session,
} from "./overlayState";

// Only call out a model load once it has run this long; warm loads finish
// well inside it, cold ones can take 40s+.
const SLOW_MODEL_LOAD_MS = 2000;
// Matches the delay in `hide_recording_overlay`, so the shape can close.
const OVERLAY_FADE_MS = 300;
// The island's close spring; the panel shrinks after it.
const CLOSE_MS = 320;
// Pill chrome around a measured label: notice (padding, badge, gap) and
// working (padding, ring, gaps, cancel).
const NOTICE_CHROME = 54;
const WORK_CHROME = 83;
const RING_LENGTH = 34.56;
const MEETING_WORK_MS = 3000;
// Stops are quick; stop ignoring clicks if `meeting-switched` never comes.
const STOP_TIMEOUT_MS = 15000;
const EMPTY_LEVELS = Array<number>(16).fill(0);

const clamp = (v: number, lo: number, hi: number) =>
  Math.min(hi, Math.max(lo, v));

const CrossIcon = () => (
  <svg viewBox="0 0 16 16" aria-hidden="true">
    <path
      d="M4 4l8 8M12 4l-8 8"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
    />
  </svg>
);

// The cat's body with its ears up. Asleep it is the idle tray glyph.
const CAT_AWAKE =
  "M0 13V11.5A3.5 3.5 0 0 1 5.29 8.49L6.18 3.69A0.4 0.4 0 0 1 6.93 3.58L8.17 6.09A4 4 0 0 1 9.83 6.09L11.07 3.58A0.4 0.4 0 0 1 11.82 3.69L12.71 8.5A4 4 0 0 1 13 10V11.56A0.44 0.44 0 0 0 13.44 12H17A1 1 0 0 1 17 14H6A1 1 0 0 1 5 13V11.5A1.5 1.5 0 0 0 2 11.5V13A1 1 0 0 1 0 13Z";

/** Dictation's mark: awake with its eyes open while it listens, else asleep. */
const Cat: React.FC<{ awake: boolean }> = ({ awake }) => (
  <svg
    className="cat"
    viewBox="0 2 18 12.4"
    width="22"
    height="15.2"
    aria-hidden="true"
  >
    <path fill="currentColor" d={awake ? CAT_AWAKE : TRAY_PATHS.idle} />
    {awake && (
      <g className="eyes">
        <rect x="6" y="9" width="2" height="2" rx=".5" />
        <rect x="10" y="9" width="2" height="2" rx=".5" />
      </g>
    )}
  </svg>
);

const Ring: React.FC<{ progress?: number }> = ({ progress }) => (
  <svg
    className={`ring ${progress === undefined ? "spin" : "prog"}`}
    viewBox="0 0 14 14"
    aria-hidden="true"
  >
    <circle className="trk" cx="7" cy="7" r="5.5" />
    <circle
      className="arc"
      cx="7"
      cy="7"
      r="5.5"
      style={
        progress === undefined
          ? undefined
          : {
              strokeDasharray: `${(progress * RING_LENGTH).toFixed(2)} ${RING_LENGTH}`,
            }
      }
    />
  </svg>
);

const Badge: React.FC<{ ok: boolean }> = ({ ok }) =>
  ok ? (
    <svg className="badge" viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="8" r="8" fill="currentColor" />
      <path
        d="M4.8 8.3l2.2 2.2 4.2-4.6"
        fill="none"
        stroke="#141312"
        strokeWidth="1.9"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  ) : (
    <svg className="badge" viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="8" r="8" fill="currentColor" />
      <path
        d="M8 4v4.6"
        stroke="#141312"
        strokeWidth="2"
        strokeLinecap="round"
      />
      <path
        d="M8 11.8v.01"
        stroke="#141312"
        strokeWidth="2.2"
        strokeLinecap="round"
      />
    </svg>
  );

const Wave: React.FC<{
  n: number;
  max: number;
  levels: number[];
  className?: string;
}> = ({ n, max, levels, className = "" }) => {
  const heights = barHeights(levels, n, max);
  return (
    <span className={`wave ${className}`}>
      {heights.map((h, i) => (
        <i
          key={i}
          style={
            {
              height: `${h.toFixed(1)}px`,
              "--d": Math.abs(i - (n - 1) / 2),
            } as React.CSSProperties
          }
        />
      ))}
    </span>
  );
};

/** Turns `on` into the mock's `pc` piece: shown, or faded and shrunk. */
const pc = (on: boolean, flat = false) =>
  `pc${flat ? " flat" : ""}${on ? " on" : ""}`;

const RecordingOverlay: React.FC = () => {
  const { t } = useTranslation();
  const [geometry, setGeometry] = useState<OverlayGeometry>({
    form: "pill",
    notch_width: null,
    notch_height: null,
  });
  const [session, setSession] = useState<Session | null>(null);
  const [captureReady, setCaptureReady] = useState(false);
  const [modelLoading, setModelLoading] = useState(false);
  const [modelLoadSlow, setModelLoadSlow] = useState(false);
  const modelLoadTimer = useRef<ReturnType<typeof setTimeout>>();
  const [levels, setLevels] = useState<number[]>(EMPTY_LEVELS);
  const smoothed = useRef<number[]>(EMPTY_LEVELS);
  const [stream, setStream] = useState<StreamTextEvent>({
    committed: "",
    tentative: "",
  });
  const [working, setWorking] = useState(false);
  const [workKind, setWorkKind] = useState<StreamWorkKind>("transcribing");
  const [edge, setEdge] = useState<"top" | "bottom">("bottom");
  const [notice, setNotice] = useState<Notice | null>(null);
  const fadeTimer = useRef<ReturnType<typeof setTimeout>>();

  // A meeting recording beside dictation, and its two sides' levels.
  const [meetingStart, setMeetingStart] = useState<number | null>(null);
  const [meetingLevels, setMeetingLevels] = useState({
    me: EMPTY_LEVELS,
    others: EMPTY_LEVELS,
  });
  const [controls, setControls] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const [meetingWork, setMeetingWork] = useState<number | null>(null);
  const meetingWorkTimer = useRef<ReturnType<typeof setTimeout>>();
  // Set from a Stop click until the backend finishes the stop, so a double
  // click can't start a new meeting.
  const stopping = useRef(false);
  const stoppingTimer = useRef<ReturnType<typeof setTimeout>>();

  const [noticeW, setNoticeW] = useState<number>();
  const [workW, setWorkW] = useState<number>();
  const [liveH, setLiveH] = useState<number>();
  const noticeProbe = useRef<HTMLSpanElement>(null);
  const workProbe = useRef<HTMLSpanElement>(null);
  const liveText = useRef<HTMLParagraphElement>(null);
  const direction = getLanguageDirection(i18n.language);

  useEffect(() => {
    const subscriptions = [
      listen<OverlayGeometry>("overlay-form", (e) => setGeometry(e.payload)),
      listen<Notice>("overlay-notice", (e) => {
        clearTimeout(fadeTimer.current);
        setNotice(e.payload);
      }),
      listen<string>("show-overlay", async (e) => {
        clearTimeout(fadeTimer.current);
        // A meeting recording or transcribing shows with no session.
        const next =
          e.payload === "meeting" || e.payload === "meeting_transcribing"
            ? null
            : (e.payload as Session);
        if (next !== "notice" && next !== "live_notice") setNotice(null);
        // Reset before the awaits below: a fast microphone can send
        // recording-ready while they run.
        if (next === "recording" || next === "streaming") {
          setCaptureReady(false);
          smoothed.current = EMPTY_LEVELS;
          setLevels(EMPTY_LEVELS);
          setStream({ committed: "", tentative: "" });
          setWorking(false);
          setWorkKind("transcribing");
        }
        await syncLanguageFromSettings();
        try {
          const settings = await commands.getAppSettings();
          if (settings.status === "ok") {
            setEdge(
              settings.data.overlay_position === "top" ? "top" : "bottom",
            );
          }
        } catch {
          // Keep the last placement.
        }
        setSession(next);
      }),
      listen("hide-overlay", () => {
        setSession(null);
        setCaptureReady(false);
        clearTimeout(fadeTimer.current);
        fadeTimer.current = setTimeout(() => {
          setNotice(null);
          setStream({ committed: "", tentative: "" });
        }, OVERLAY_FADE_MS);
      }),
      listen("recording-ready", () => setCaptureReady(true)),
      listen<number[]>("mic-level", (e) => {
        smoothed.current = smoothed.current.map(
          (prev, i) => prev * 0.7 + (e.payload[i] || 0) * 0.3,
        );
        setLevels(smoothed.current);
      }),
      listen("meeting-switched", () => {
        clearTimeout(stoppingTimer.current);
        stopping.current = false;
      }),
      listen<{ started_at_ms: number | null }>("overlay-meeting", (e) => {
        setMeetingStart(e.payload.started_at_ms);
        if (e.payload.started_at_ms === null) setControls(false);
      }),
      // Each side's loudest sample, 30 times a second; the bars scroll.
      events.meetingLevels.listen((e) =>
        setMeetingLevels((l) => ({
          me: [...l.me.slice(1), e.payload.mic],
          others: [...l.others.slice(1), e.payload.system],
        })),
      ),
      // The backend shows the overlay with `meeting_transcribing` first;
      // this holds the pill as long as it does. Telling speakers apart
      // sends no percent, so the last one stays.
      events.meetingProgress.listen((e) => {
        setMeetingWork((w) => e.payload.percent ?? w ?? 0);
        clearTimeout(meetingWorkTimer.current);
        meetingWorkTimer.current = setTimeout(
          () => setMeetingWork(null),
          MEETING_WORK_MS,
        );
      }),
      events.streamTextEvent.listen((e) => setStream(e.payload)),
      events.streamPhaseEvent.listen((e) => {
        setWorking(e.payload.phase === "working");
        if (e.payload.kind) setWorkKind(e.payload.kind);
      }),
      // Every `loading_started` ends in exactly one completed or failed.
      listen<ModelStateEvent>("model-state-changed", (e) => {
        const type = e.payload.event_type;
        if (type === "loading_started") {
          clearTimeout(modelLoadTimer.current);
          setModelLoading(true);
          setModelLoadSlow(false);
          modelLoadTimer.current = setTimeout(
            () => setModelLoadSlow(true),
            SLOW_MODEL_LOAD_MS,
          );
        } else if (type === "loading_completed" || type === "loading_failed") {
          clearTimeout(modelLoadTimer.current);
          setModelLoading(false);
          setModelLoadSlow(false);
        }
      }),
    ];
    return () => {
      subscriptions.forEach((s) => s.then((off) => off()));
      clearTimeout(modelLoadTimer.current);
      clearTimeout(fadeTimer.current);
      clearTimeout(meetingWorkTimer.current);
      clearTimeout(stoppingTimer.current);
    };
  }, []);

  const meeting = meetingStart !== null;
  useEffect(() => {
    if (!meeting) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [meeting]);

  const live = session === "streaming" || session === "live_notice";
  // While listening, a slow model load fills the text area with a note, so
  // speech that isn't transcribing yet doesn't look ignored.
  const loadingNote = live && !working && modelLoadSlow;
  const hasText =
    stream.committed.length > 0 || stream.tentative.length > 0 || loadingNote;
  const state = visualState({
    session,
    captureReady,
    working,
    hasText: live && hasText,
    noticeLevel: notice?.level ?? null,
    meeting,
    controls,
    meetingWork: meetingWork !== null,
  });

  const noticeTitle = notice ? t(noticeDisplay(notice.code).title) : "";
  // Notices have no "info" level yet; a finished meeting will bring one.
  const level = notice?.level as string | undefined;
  const levelAttr = level === "warning" ? "warn" : level;
  const workLabel =
    state === "processing" || (working && workKind === "polishing")
      ? t("overlay.processing")
      : modelLoading
        ? t("overlay.loadingModel")
        : t("overlay.transcribing");

  // CSS can't animate to an intrinsic width, so labels are measured.
  useLayoutEffect(() => {
    const n = noticeProbe.current?.getBoundingClientRect().width;
    if (n) setNoticeW(clamp(Math.ceil(n) + NOTICE_CHROME, 150, 420));
    const w = workProbe.current?.getBoundingClientRect().width;
    if (w) setWorkW(clamp(Math.ceil(w) + WORK_CHROME, 150, 300));
  }, [noticeTitle, workLabel]);

  // The text area follows the text, one to three lines.
  useLayoutEffect(() => {
    const p = liveText.current;
    if (p) setLiveH(p.offsetHeight);
  }, [stream, loadingNote, geometry.form]);

  const toggleControls = async () => {
    if (!meeting) return;
    if (controls) {
      setControls(false);
      setTimeout(() => commands.setOverlayDropped(false), CLOSE_MS);
    } else {
      await commands.setOverlayDropped(true);
      setControls(true);
    }
  };

  const cancel = (e: React.MouseEvent) => {
    e.stopPropagation();
    commands.cancelOperation();
  };
  const stopMeeting = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (stopping.current) return;
    stopping.current = true;
    stoppingTimer.current = setTimeout(() => {
      stopping.current = false;
    }, STOP_TIMEOUT_MS);
    commands.setMeetingRecording(false);
  };
  const dismissNotice = state.startsWith("notice")
    ? () => commands.dismissOverlayNotice()
    : undefined;

  const timer = formatTimer(meeting ? (now - meetingStart) / 1000 : 0);
  const percent = meetingWork ?? 0;
  const dotState =
    state === "arming"
      ? "arming"
      : state === "listening" || state === "live"
        ? "live"
        : state.startsWith("meeting") && state !== "meeting-transcribing"
          ? "rec"
          : "held";
  const waveState = state === "arming" ? "arming" : "live";
  const mixed = meetingLevels.me.map((v, i) =>
    Math.max(v, meetingLevels.others[i] ?? 0),
  );

  const liveContent = (
    <p className="livetext" ref={liveText}>
      {loadingNote && !stream.committed && !stream.tentative ? (
        <span className="tentative">{t("overlay.loadingModel")}</span>
      ) : (
        <>
          <span className="committed">
            {stream.committed ? `${stream.committed} ` : ""}
          </span>
          <span className="tentative">{stream.tentative}</span>
        </>
      )}
      {state === "live" && <span className="caret" />}
    </p>
  );

  const probes = (
    <>
      <span ref={noticeProbe} className="pc-probe label" aria-hidden="true">
        {noticeTitle}
      </span>
      <span ref={workProbe} className="pc-probe label" aria-hidden="true">
        {workLabel}
      </span>
    </>
  );

  if (geometry.form === "island") {
    const nw = geometry.notch_width ?? 185;
    const nh = geometry.notch_height ?? 32;
    const leftEar =
      state === "meeting-transcribing"
        ? "prog"
        : state === "idle" || state.startsWith("notice")
          ? null
          : "dot";
    const rightEar =
      state === "transcribing" ||
      state === "processing" ||
      state === "live-working"
        ? "spin"
        : state === "meeting" || state === "meeting-controls"
          ? "timer"
          : state === "meeting-transcribing"
            ? "pct"
            : state === "idle" || state.startsWith("notice")
              ? null
              : "wave";
    const drop = state.startsWith("notice")
      ? "notice"
      : state.startsWith("live")
        ? "live"
        : state === "meeting-controls"
          ? "meeting"
          : null;
    return (
      <div
        dir={direction}
        className="ov stage"
        style={
          {
            "--nw": `${nw}px`,
            "--nh": `${nh}px`,
            "--live-h": liveH ? `${Math.min(liveH, 60) + 20}px` : undefined,
          } as React.CSSProperties
        }
      >
        {probes}
        <div
          className="island"
          role="status"
          data-state={state}
          data-dot={dotState}
          data-wave={waveState}
          data-level={levelAttr}
          onClick={dismissNotice ?? toggleControls}
        >
          <div className="body">
            <div className="ear ear-l">
              <span className={pc(leftEar === "dot" && dotState !== "rec")}>
                <Cat awake={dotState === "live"} />
              </span>
              <span className={pc(leftEar === "dot" && dotState === "rec")}>
                <i className="dot" />
              </span>
              <button
                type="button"
                className="xbtn"
                aria-label={t("overlay.cancel")}
                onClick={cancel}
              >
                <CrossIcon />
              </button>
              <span className={pc(leftEar === "prog")}>
                <Ring progress={percent / 100} />
              </span>
            </div>
            <div className="ear ear-r">
              <span className={pc(rightEar === "wave")}>
                <Wave n={5} max={17} levels={levels} className="w5" />
              </span>
              <span className={pc(rightEar === "spin")}>
                <Ring />
              </span>
              <span className={`timer ${pc(rightEar === "timer")}`}>
                {timer}
              </span>
              <span className={`pct ${pc(rightEar === "pct")}`}>
                {t("overlay.percent", { percent: Math.round(percent) })}
              </span>
            </div>
            <div className="drop">
              <div className={`d-notice ${pc(drop === "notice", true)}`}>
                <Badge ok={level === "info"} />
                <span>{noticeTitle}</span>
              </div>
              <div
                className={`d-live ${pc(drop === "live", true)}`}
                style={{ height: liveH ? Math.min(liveH, 60) + 20 : undefined }}
              >
                {liveContent}
              </div>
              <div className={`d-meeting ${pc(drop === "meeting", true)}`}>
                <span className="meter">
                  {t("overlay.me")}
                  <Wave n={7} max={14} levels={meetingLevels.me} />
                </span>
                <span className="meter">
                  {t("overlay.others")}
                  <Wave
                    n={7}
                    max={14}
                    levels={meetingLevels.others}
                    className="neutral"
                  />
                </span>
                <button
                  type="button"
                  className="stopwide"
                  onClick={stopMeeting}
                >
                  {t("overlay.stop")}
                </button>
              </div>
            </div>
          </div>
          <span className="edge" />
        </div>
      </div>
    );
  }

  const scene =
    state === "arming" || state === "listening" || state === "live"
      ? "listen"
      : state === "transcribing" ||
          state === "processing" ||
          state === "live-working"
        ? "work"
        : state === "meeting" || state === "meeting-controls"
          ? "meet"
          : state === "meeting-dictating"
            ? "meetdict"
            : state === "meeting-transcribing"
              ? "meetwork"
              : state.startsWith("notice")
                ? "notice"
                : null;

  return (
    <div
      dir={direction}
      className="ov stage"
      style={
        {
          "--notice-w": noticeW ? `${noticeW}px` : undefined,
          "--work-w": workW ? `${workW}px` : undefined,
          "--live-h": liveH ? `${Math.min(liveH, 72)}px` : undefined,
        } as React.CSSProperties
      }
    >
      {probes}
      <div
        className="pill"
        role="status"
        data-edge={edge}
        data-state={state}
        data-dot={dotState}
        data-wave={waveState}
        data-level={levelAttr}
        onClick={dismissNotice}
      >
        <div className="p-text">{liveContent}</div>
        <div className="p-row">
          <div className={`sc sc-listen ${pc(scene === "listen")}`}>
            <Cat awake={dotState === "live"} />
            <Wave n={13} max={20} levels={levels} />
            <button
              type="button"
              className="xbtn"
              aria-label={t("overlay.cancel")}
              onClick={cancel}
            >
              <CrossIcon />
            </button>
          </div>
          <div className={`sc sc-work ${pc(scene === "work")}`}>
            <Ring />
            <span className="label">{workLabel}</span>
            <button
              type="button"
              className="xbtn"
              aria-label={t("overlay.cancel")}
              onClick={cancel}
            >
              <CrossIcon />
            </button>
          </div>
          <div className={`sc sc-meet ${pc(scene === "meet")}`}>
            <i className="dot" />
            <span className="timer">{timer}</span>
            <Wave n={9} max={18} levels={mixed} className="neutral" />
            <button
              type="button"
              className="stopbtn"
              aria-label={t("overlay.stopMeeting")}
              onClick={stopMeeting}
            />
          </div>
          <div className={`sc sc-meetdict ${pc(scene === "meetdict")}`}>
            <i className="dot" />
            <span className="timer">{timer}</span>
            <i className="divider" />
            <Wave n={13} max={20} levels={levels} />
            <button
              type="button"
              className="xbtn"
              aria-label={t("overlay.cancel")}
              onClick={cancel}
            >
              <CrossIcon />
            </button>
          </div>
          <div className={`sc sc-meetwork ${pc(scene === "meetwork")}`}>
            <Ring progress={percent / 100} />
            <span className="label">{t("overlay.meetingTranscribing")}</span>
            <span className="pct muted">
              {t("overlay.percent", { percent: Math.round(percent) })}
            </span>
          </div>
          <div className={`sc sc-notice ${pc(scene === "notice")}`}>
            <Badge ok={level === "info"} />
            <span className="n-title">{noticeTitle}</span>
          </div>
        </div>
      </div>
    </div>
  );
};

export default RecordingOverlay;
