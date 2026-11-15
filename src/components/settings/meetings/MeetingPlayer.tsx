import React, { useRef } from "react";
import { useTranslation } from "react-i18next";
import type { MeetingSegment } from "@/bindings";
import { formatClock, speakerSlot } from "./meetingModel";
import type { MeetingPlayback } from "./useMeetingPlayback";

/** Arrow keys on the lanes move this far. */
const STEP_S = 5;

const PlayIcon = () => (
  <svg viewBox="0 0 24 24" aria-hidden="true">
    <path d="M7 4v16l13-8z" />
  </svg>
);
const PauseIcon = () => (
  <svg viewBox="0 0 24 24" aria-hidden="true">
    <rect x="5" y="4" width="5" height="16" rx="1" />
    <rect x="14" y="4" width="5" height="16" rx="1" />
  </svg>
);

interface MeetingPlayerProps {
  playback: MeetingPlayback;
  durationS: number;
  /** One lane per speaker, in this order; none while there's no transcript. */
  lanes: string[];
  segments: MeetingSegment[];
  /** Meeting seconds where the search matched, drawn above the lanes. */
  hits?: number[];
}

/** Play, time, and the meeting as lanes: one per speaker, blocks where they talk. */
export const MeetingPlayer: React.FC<MeetingPlayerProps> = ({
  playback,
  durationS,
  lanes,
  segments,
  hits = [],
}) => {
  const { t } = useTranslation();
  const lanesRef = useRef<HTMLDivElement>(null);
  const pct = (s: number) =>
    `${(durationS ? (s / durationS) * 100 : 0).toFixed(3)}%`;

  const seekAt = (clientX: number) => {
    const el = lanesRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const rtl = getComputedStyle(el).direction === "rtl";
    const x = rtl ? r.right - clientX : clientX - r.left;
    playback.seek((x / r.width) * durationS);
  };

  const choiceLabel = {
    both: t("meetings.player.both"),
    me: t("meetings.player.me"),
    others: t("meetings.player.others"),
  }[playback.choice];

  return (
    <div className="player">
      <button
        type="button"
        className="playbtn"
        disabled={playback.loading}
        aria-label={
          playback.playing
            ? t("meetings.player.pause")
            : t("meetings.player.play")
        }
        onClick={() => (playback.playing ? playback.pause() : playback.play())}
      >
        {playback.playing ? <PauseIcon /> : <PlayIcon />}
      </button>
      <span className="clock">
        <b>{formatClock(playback.t)}</b> / {formatClock(durationS)}
      </span>
      <div
        ref={lanesRef}
        className="lanes"
        role="slider"
        tabIndex={0}
        aria-label={t("meetings.player.position")}
        aria-valuemin={0}
        aria-valuemax={Math.floor(durationS)}
        aria-valuenow={Math.floor(playback.t)}
        aria-valuetext={formatClock(playback.t)}
        onPointerDown={(e) => {
          e.currentTarget.setPointerCapture(e.pointerId);
          seekAt(e.clientX);
        }}
        onPointerMove={(e) => {
          if (e.currentTarget.hasPointerCapture(e.pointerId)) seekAt(e.clientX);
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowRight") playback.seek(playback.t + STEP_S);
          else if (e.key === "ArrowLeft") playback.seek(playback.t - STEP_S);
          else return;
          e.preventDefault();
        }}
      >
        <div className="hits" aria-hidden="true">
          {hits.map((s, i) => (
            <i key={i} style={{ insetInlineStart: pct(s) }} />
          ))}
        </div>
        {lanes.length === 0 ? (
          <div className="lane" />
        ) : (
          lanes.map((key) => (
            <div key={key} className="lane" data-sp={speakerSlot(key)}>
              {segments
                .filter((s) => s.speaker_key === key)
                .map((s) => (
                  <b
                    key={s.t0_ms}
                    style={{
                      insetInlineStart: pct(s.t0_ms / 1000),
                      width: pct((s.t1_ms - s.t0_ms) / 1000),
                    }}
                  />
                ))}
            </div>
          ))
        )}
        <i className="head" style={{ insetInlineStart: pct(playback.t) }} />
      </div>
      <button
        type="button"
        className="pbtn"
        title={t("meetings.player.speed")}
        onClick={playback.cycleSpeed}
      >
        {t("meetings.player.speedValue", { speed: playback.speed })}
      </button>
      {playback.canChoose && (
        <button
          type="button"
          className="pbtn"
          title={t("meetings.player.choice")}
          onClick={playback.cycleChoice}
        >
          {choiceLabel}
        </button>
      )}
    </div>
  );
};
