import React, {
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  ChevronLeft,
  ExternalLink,
  MoreHorizontal,
  Search,
} from "lucide-react";
import {
  commands,
  events,
  type MeetingDetail as Detail,
  type MeetingRow,
  type MeetingSpeaker,
} from "@/bindings";
import { Button } from "@/components/ui/Button";
import { ProgressLine } from "@/components/shared/ProgressLine";
import { notify } from "@/stores/noticeStore";
import { copyToClipboard } from "../history/clipboard";
import { Highlight } from "./Highlight";
import { MeetingPlayer } from "./MeetingPlayer";
import { StartStopButton } from "./MeetingList";
import { useMeetingsStore } from "./meetingsStore";
import {
  MEETING_STEPS,
  formatClock,
  formatDuration,
  isUnnamed,
  languageName,
  matchRanges,
  meetingDate,
  meetingStatus,
  meetingTitle,
  segmentAt,
  speakerName,
  speakerOrder,
  speakerSlot,
  startedAt,
  talkShares,
  toTurns,
} from "./meetingModel";
import { useMeetingPlayback } from "./useMeetingPlayback";

/** Bars in each level track of a recording meeting. */
const METER_BARS = 40;

const Header: React.FC<{
  row: MeetingRow;
  onBack: () => void;
  actions?: React.ReactNode;
  children?: React.ReactNode;
}> = ({ row, onBack, actions, children }) => {
  const { t, i18n } = useTranslation();
  const refresh = useMeetingsStore((s) => s.refresh);
  const fallback = meetingTitle(t, i18n.language, { ...row, title: null });
  const [title, setTitle] = useState(row.title ?? "");
  useEffect(() => setTitle(row.title ?? ""), [row.id, row.title]);
  // Escape blurs the field, and blur saves; this tells the blur not to.
  const cancelled = useRef(false);

  const save = async () => {
    if (cancelled.current) {
      cancelled.current = false;
      return;
    }
    const next = title.trim();
    if (next === (row.title ?? "")) return;
    const result = await commands.renameMeeting(row.id, next || null);
    if (result.status === "error") {
      notify({ title: t("meetings.renameFailed"), detail: result.error });
      setTitle(row.title ?? "");
    }
    refresh();
  };

  const status = meetingStatus(row);
  const meta = [
    meetingDate(i18n.language, row),
    status === "recording" ? "" : formatDuration(t, row.duration_ms),
    languageName(i18n.language, row.language),
  ].filter(Boolean);

  return (
    <header className="d-head">
      <button type="button" className="back" onClick={onBack}>
        <ChevronLeft className="ic" aria-hidden="true" />
        {t("meetings.title")}
      </button>
      <div className="d-top">
        <input
          className="title-input"
          aria-label={t("meetings.titleLabel")}
          value={title}
          placeholder={fallback}
          onChange={(e) => setTitle(e.target.value)}
          onBlur={save}
          onKeyDown={(e) => {
            if (e.key === "Enter") e.currentTarget.blur();
            if (e.key === "Escape") {
              cancelled.current = true;
              setTitle(row.title ?? "");
              e.currentTarget.blur();
            }
          }}
        />
        {actions}
      </div>
      <div className="meta">
        {meta.map((m) => (
          <span key={m}>{m}</span>
        ))}
        {status === "done" &&
          (row.note_path ? (
            <span className="path" title={row.note_path}>
              {t("meetings.note", { path: row.note_path })}
            </span>
          ) : (
            <span className="path">{t("meetings.noNote")}</span>
          ))}
      </div>
      {children}
    </header>
  );
};

const transcriptText = (t: TFunction, detail: Detail) => {
  const labels = new Map(detail.speakers.map((s) => [s.key, s.label]));
  return toTurns(detail.segments)
    .map((turn) => {
      const name = speakerName(t, turn.speaker, labels.get(turn.speaker));
      const text = turn.segments.map((i) => detail.segments[i].text).join(" ");
      return `[${formatClock(turn.t0_ms / 1000)}] ${name}: ${text}`;
    })
    .join("\n\n");
};

/** Queues meeting `id` again; the list shows it transcribing once it is. */
const rerun = async (t: TFunction, id: string, speakersOnly = false) => {
  const result = speakersOnly
    ? await commands.rediarizeMeeting(id)
    : await commands.retranscribeMeeting(id);
  if (result.status === "error")
    notify({ title: t("meetings.rerunFailed"), detail: result.error });
};

const MoreMenu: React.FC<{ detail: Detail }> = ({ detail }) => {
  const { t } = useTranslation();
  const refresh = useMeetingsStore((s) => s.refresh);
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: PointerEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);

  const { id, dir, note_path } = detail.meeting;
  const item = (label: string, run: () => unknown) => (
    <button
      type="button"
      role="menuitem"
      onClick={() => {
        setOpen(false);
        run();
      }}
    >
      {label}
    </button>
  );
  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        className="iconbtn"
        aria-label={t("meetings.more")}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <MoreHorizontal className="ic" aria-hidden="true" />
      </button>
      {open && (
        <div className="menu" role="menu">
          <button
            type="button"
            role="menuitem"
            onClick={async () => {
              setOpen(false);
              const ok = await copyToClipboard(transcriptText(t, detail));
              if (!ok) notify({ title: t("meetings.copyFailed") });
            }}
          >
            {t("meetings.copyTranscript")}
          </button>
          {dir &&
            item(t("meetings.openFolder"), () =>
              revealItemInDir(dir).catch(console.error),
            )}
          {!note_path &&
            item(t("meetings.writeNote"), async () => {
              const result = await commands.exportMeetingNote(id);
              if (result.status === "error")
                notify({
                  title: t("meetings.writeNoteFailed"),
                  detail: result.error,
                });
              refresh();
            })}
          {item(t("meetings.rediarize"), () => rerun(t, id, true))}
          {item(t("meetings.retranscribe"), () => rerun(t, id))}
        </div>
      )}
    </div>
  );
};

const SpeakerChip: React.FC<{
  meetingId: string;
  speaker: MeetingSpeaker;
  share: number;
  onRenamed: () => void;
}> = ({ meetingId, speaker, share, onRenamed }) => {
  const { t } = useTranslation();
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState("");
  const shown = speakerName(t, speaker.key, speaker.label);

  const finish = async (save: boolean) => {
    setEditing(false);
    if (!save || name.trim() === (speaker.label ?? "")) return;
    const result = await commands.renameSpeaker(
      meetingId,
      speaker.key,
      name.trim() || null,
    );
    if (result.status === "error") {
      notify({ title: t("meetings.renameFailed"), detail: result.error });
    }
    onRenamed();
  };

  return (
    <button
      type="button"
      className={`chip ${isUnnamed(speaker) ? "unnamed" : ""} ${editing ? "editing" : ""}`}
      data-sp={speakerSlot(speaker.key)}
      title={t("meetings.rename")}
      onClick={() => {
        if (editing) return;
        setName(speaker.label ?? "");
        setEditing(true);
      }}
    >
      <i aria-hidden="true" />
      {editing ? (
        <input
          autoFocus
          aria-label={t("meetings.speakerName")}
          value={name}
          placeholder={shown}
          onFocus={(e) => e.currentTarget.select()}
          onChange={(e) => setName(e.target.value)}
          onBlur={() => finish(true)}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") finish(true);
            if (e.key === "Escape") finish(false);
          }}
        />
      ) : (
        <span>{shown}</span>
      )}
      <small>{t("meetings.share", { percent: share })}</small>
    </button>
  );
};

/** A recording meeting: the time, both sides' levels, and Stop. */
const RecordingView: React.FC<{ row: MeetingRow }> = ({ row }) => {
  const { t } = useTranslation();
  const [now, setNow] = useState(() => Date.now());
  const [levels, setLevels] = useState({
    me: Array<number>(METER_BARS).fill(0),
    others: Array<number>(METER_BARS).fill(0),
  });

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    // Each side's loudest sample, 30 times a second; the bars scroll.
    const off = events.meetingLevels.listen((e) =>
      setLevels((l) => ({
        me: [...l.me.slice(1), e.payload.mic],
        others: [...l.others.slice(1), e.payload.system],
      })),
    );
    return () => {
      clearInterval(id);
      off.then((f) => f());
    };
  }, []);

  const start = startedAt(row)?.getTime() ?? now;
  const meter = (values: number[], sp: string) => (
    <span className="meterbar" data-sp={sp} aria-hidden="true">
      {values.map((v, i) => (
        <i key={i} style={{ height: `${3 + Math.min(1, v) * 17}px` }} />
      ))}
    </span>
  );
  return (
    <div className="recview">
      <div className="bigtime" role="timer">
        <i aria-hidden="true" />
        <span>{formatClock((now - start) / 1000)}</span>
      </div>
      <div className="tracks">
        <span>{t("meetings.me")}</span>
        {meter(levels.me, "me")}
        <span>{t("meetings.others")}</span>
        {meter(levels.others, "s1")}
      </div>
      <p>{t("meetings.recordingBody")}</p>
      <StartStopButton size="md" />
    </div>
  );
};

const TranscriptSkeleton = () => (
  <div className="transcript" aria-hidden="true">
    {[88, 64, 79].map((w) => (
      <div key={w} className="t-skel">
        <span className="skel" style={{ height: 11, width: "12%" }} />
        <span className="skel" style={{ height: 11, width: `${w}%` }} />
        <span className="skel" style={{ height: 11, width: `${w - 30}%` }} />
      </div>
    ))}
  </div>
);

const TranscribingStatus: React.FC<{ id: string }> = ({ id }) => {
  const { t } = useTranslation();
  const progress = useMeetingsStore((s) => s.progress[id]);
  const percent = progress?.percent ?? null;
  const at = progress ? MEETING_STEPS.indexOf(progress.step) : -1;
  return (
    <div className="status" role="status">
      <h3>
        {t("meetings.transcribing")}
        {percent !== null && (
          <span>{t("meetings.percent", { percent: Math.round(percent) })}</span>
        )}
      </h3>
      <ProgressLine value={(percent ?? 30) / 100} busy={percent === null} />
      <ol className="steps">
        {MEETING_STEPS.map((step, i) => (
          <li key={step} className={i < at ? "done" : i === at ? "on" : ""}>
            {t(`meetings.steps.${step}`)}
          </li>
        ))}
      </ol>
      <p>{t("meetings.transcribingBody")}</p>
    </div>
  );
};

interface MeetingDetailProps {
  row: MeetingRow;
  onBack: () => void;
}

export const MeetingDetail: React.FC<MeetingDetailProps> = ({
  row,
  onBack,
}) => {
  const { t } = useTranslation();
  const recordingId = useMeetingsStore((s) => s.recordingId);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const status = meetingStatus(row);
  const durationS = (row.duration_ms ?? 0) / 1000;
  const playback = useMeetingPlayback(row.id, durationS, status);

  // Read again when the meeting changes state or a speaker is renamed.
  const [version, setVersion] = useState(0);
  useEffect(() => {
    let stale = false;
    commands.getMeeting(row.id).then((r) => {
      if (stale) return;
      if (r.status === "ok") {
        setDetail(r.data);
        setLoadError(null);
      } else setLoadError(r.error);
    });
    return () => {
      stale = true;
    };
  }, [row.id, row.status, row.title, row.note_path, version]);

  if (status === "recording" && row.id === recordingId) {
    return (
      <>
        <Header row={row} onBack={onBack} />
        <RecordingView row={row} />
      </>
    );
  }
  if (status === "transcribing") {
    return (
      <>
        <Header row={row} onBack={onBack} />
        <TranscribingStatus id={row.id} />
        <TranscriptSkeleton />
        <MeetingPlayer
          playback={playback}
          durationS={durationS}
          lanes={[]}
          segments={[]}
        />
      </>
    );
  }
  if (status !== "done") {
    // Failed, or a recording a crash cut short.
    return (
      <>
        <Header row={row} onBack={onBack} />
        <div className="failbox" role="alert">
          <h3>
            {status === "failed"
              ? t("meetings.failed")
              : t("meetings.interrupted")}
          </h3>
          <p>{t("meetings.failedBody")}</p>
          <div className="rowbtns">
            <Button
              variant="secondary"
              size="md"
              onClick={() => rerun(t, row.id)}
            >
              {t("meetings.tryAgain")}
            </Button>
            {row.dir && (
              <Button
                variant="secondary"
                size="md"
                onClick={() => revealItemInDir(row.dir!).catch(console.error)}
              >
                {t("meetings.openFolder")}
              </Button>
            )}
          </div>
        </div>
        <div className="transcript" />
        <MeetingPlayer
          playback={playback}
          durationS={durationS}
          lanes={[]}
          segments={[]}
        />
      </>
    );
  }
  if (loadError) {
    return (
      <>
        <Header row={row} onBack={onBack} />
        <div className="failbox" role="alert">
          <h3>{t("meetings.loadFailedTitle")}</h3>
          <p>{t("meetings.loadFailedBody", { error: loadError })}</p>
        </div>
      </>
    );
  }
  if (!detail) return <DetailSkeleton />;
  return (
    <DoneView
      detail={detail}
      row={row}
      onBack={onBack}
      onChanged={() => setVersion((v) => v + 1)}
      playback={playback}
      durationS={durationS}
    />
  );
};

export const DetailSkeleton = () => (
  <>
    <header className="d-head" aria-busy="true">
      <span className="skel" style={{ height: 22, width: "46%" }} />
      <span
        className="skel"
        style={{ height: 11, width: "30%", marginTop: 12 }}
      />
      <span
        className="skel"
        style={{ height: 24, width: "62%", marginTop: 18, borderRadius: 12 }}
      />
    </header>
    <TranscriptSkeleton />
  </>
);

const DoneView: React.FC<{
  detail: Detail;
  row: MeetingRow;
  onBack: () => void;
  onChanged: () => void;
  playback: ReturnType<typeof useMeetingPlayback>;
  durationS: number;
}> = ({ detail, row, onBack, onChanged, playback, durationS }) => {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [hit, setHit] = useState(0);
  const searchRef = useRef<HTMLInputElement>(null);
  const transcriptRef = useRef<HTMLDivElement>(null);
  const segments = detail.segments;

  const speakers = useMemo(
    () => [...detail.speakers].sort((a, b) => speakerOrder(a.key, b.key)),
    [detail.speakers],
  );
  const labels = useMemo(
    () => new Map(detail.speakers.map((s) => [s.key, s.label])),
    [detail.speakers],
  );
  const shares = useMemo(() => talkShares(segments), [segments]);
  const turns = useMemo(() => toTurns(segments), [segments]);

  // Every match in reading order, as [segment, match within it].
  const matches = useMemo(() => {
    const out: [number, number][] = [];
    segments.forEach((s, i) =>
      matchRanges(s.text, query).forEach((_, j) => out.push([i, j])),
    );
    return out;
  }, [segments, query]);
  const current = matches.length ? matches[hit % matches.length] : null;

  const nowIndex = segmentAt(segments, playback.t * 1000);
  const nowTurn =
    nowIndex < 0 ? -1 : turns.findIndex((tn) => tn.segments.includes(nowIndex));

  useLayoutEffect(() => {
    transcriptRef.current
      ?.querySelector("mark.cur")
      ?.scrollIntoView({ block: "nearest" });
  }, [current]);
  useEffect(() => {
    if (!playback.playing || nowIndex < 0) return;
    transcriptRef.current
      ?.querySelector(`[data-seg="${nowIndex}"]`)
      ?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [nowIndex, playback.playing]);

  // Space plays, Ctrl/Cmd+F searches this meeting.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        return;
      }
      const tag = (document.activeElement as HTMLElement | null)?.tagName;
      if (e.key === " " && !/INPUT|TEXTAREA|BUTTON|SELECT/.test(tag ?? "")) {
        e.preventDefault();
        if (playback.playing) playback.pause();
        else playback.play();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [playback]);

  const notSeparated = detail.speakers.some((s) => s.key === "them");
  const note = row.note_path;
  const actions = (
    <div className="actions">
      <Button
        variant="secondary"
        size="md"
        className="inline-flex items-center gap-[7px]"
        disabled={!note}
        title={note ?? t("meetings.noNote")}
        onClick={() =>
          note &&
          openUrl(`obsidian://open?path=${encodeURIComponent(note)}`).catch(
            console.error,
          )
        }
      >
        <ExternalLink className="ic" aria-hidden="true" />
        {t("meetings.openInObsidian")}
      </Button>
      <MoreMenu detail={detail} />
    </div>
  );

  return (
    <>
      <Header row={row} onBack={onBack} actions={actions}>
        <div className="d-tools">
          <div
            className="chips"
            role="group"
            aria-label={t("meetings.speakersGroup")}
          >
            {speakers.map((s) => (
              <SpeakerChip
                key={s.key}
                meetingId={row.id}
                speaker={s}
                share={shares.get(s.key) ?? 0}
                onRenamed={onChanged}
              />
            ))}
          </div>
          <label className="field">
            <Search className="ic" aria-hidden="true" />
            <input
              ref={searchRef}
              type="search"
              value={query}
              placeholder={t("meetings.searchMeeting")}
              aria-label={t("meetings.searchMeeting")}
              onChange={(e) => {
                setQuery(e.target.value);
                setHit(0);
              }}
              onKeyDown={(e) => {
                if (e.key !== "Enter" || !matches.length) return;
                setHit((h) =>
                  e.shiftKey
                    ? (h - 1 + matches.length) % matches.length
                    : (h + 1) % matches.length,
                );
              }}
            />
            <span className="count" aria-live="polite">
              {query.trim()
                ? matches.length
                  ? t("meetings.hitCount", {
                      current: (hit % matches.length) + 1,
                      total: matches.length,
                    })
                  : t("meetings.noHits")
                : ""}
            </span>
          </label>
        </div>
        {notSeparated && (
          <p className="hintline">{t("meetings.notSeparated")}</p>
        )}
      </Header>
      <div className="transcript" ref={transcriptRef}>
        {turns.length === 0 && (
          <p className="hintline">{t("meetings.noTranscript")}</p>
        )}
        {turns.map((turn, ti) => (
          <div
            key={turn.t0_ms}
            className={`turn ${ti === nowTurn ? "now" : ""}`}
            data-sp={speakerSlot(turn.speaker)}
          >
            <button
              type="button"
              className="ts"
              aria-label={t("meetings.playFrom", {
                time: formatClock(turn.t0_ms / 1000),
              })}
              onClick={() => playback.play(turn.t0_ms / 1000)}
            >
              {formatClock(turn.t0_ms / 1000)}
            </button>
            <div className="said">
              <div className="who">
                {speakerName(t, turn.speaker, labels.get(turn.speaker))}
              </div>
              <p className="text">
                {turn.segments.map((i) => (
                  <React.Fragment key={i}>
                    <span
                      className={`seg ${i === nowIndex ? "now" : ""}`}
                      data-seg={i}
                      onClick={() => {
                        if (!String(window.getSelection() ?? "")) {
                          playback.play(segments[i].t0_ms / 1000);
                        }
                      }}
                    >
                      <Highlight
                        text={segments[i].text}
                        query={query}
                        current={current?.[0] === i ? current[1] : -1}
                      />
                    </span>{" "}
                  </React.Fragment>
                ))}
              </p>
            </div>
          </div>
        ))}
      </div>
      <MeetingPlayer
        playback={playback}
        durationS={durationS}
        lanes={speakers.map((s) => s.key)}
        segments={segments}
        hits={matches.map(([i]) => segments[i].t0_ms / 1000)}
      />
    </>
  );
};
