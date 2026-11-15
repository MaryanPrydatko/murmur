import React, { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, Search } from "lucide-react";
import { commands, type MeetingHit, type MeetingRow } from "@/bindings";
import { ProgressLine } from "@/components/shared/ProgressLine";
import { Button } from "@/components/ui/Button";
import { Highlight } from "./Highlight";
import { useMeetingsStore } from "./meetingsStore";
import {
  dayGroup,
  dayGroupKey,
  formatClock,
  formatDuration,
  formatTime,
  meetingStatus,
  meetingTitle,
  speakerOrder,
  speakerSlot,
  startedAt,
  type DayGroup,
} from "./meetingModel";

/** Characters kept before a match in a row's snippet. */
const SNIPPET_LEAD = 14;

export const StartStopButton: React.FC<{ size?: "sm" | "md" }> = ({
  size = "sm",
}) => {
  const { t } = useTranslation();
  const recording = useMeetingsStore((s) => s.recordingId !== null);
  const pending = useMeetingsStore((s) => s.pending);
  const toggle = useMeetingsStore((s) => s.toggle);
  return (
    <Button
      variant={recording ? "danger" : "primary"}
      size={size}
      className="inline-flex items-center gap-[7px]"
      disabled={pending}
      aria-busy={pending}
      onClick={() => toggle()}
    >
      {pending ? (
        <Loader2 className="-m-0.5 size-3 animate-spin" aria-hidden="true" />
      ) : (
        <i className={`recdot ${recording ? "sq" : ""}`} aria-hidden="true" />
      )}
      {recording ? t("meetings.stop") : t("meetings.start")}
    </Button>
  );
};

const groupLabel = (t: (k: string) => string, g: DayGroup) =>
  g.kind === "month" ? g.label : t(`meetings.day.${g.kind}`);

/** Ticks the elapsed time of a recording meeting. */
const useNow = (active: boolean) => {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [active]);
  return now;
};

const RowMeta: React.FC<{ row: MeetingRow; hit?: MeetingHit; q: string }> = ({
  row,
  hit,
  q,
}) => {
  const { t, i18n } = useTranslation();
  const recordingId = useMeetingsStore((s) => s.recordingId);
  const progress = useMeetingsStore((s) => s.progress[row.id]);
  const keys = useMeetingsStore((s) => s.speakers[row.id]);
  const status = meetingStatus(row);
  const live = status === "recording" && row.id === recordingId;
  const now = useNow(live);

  if (live) {
    const start = startedAt(row)?.getTime() ?? now;
    return (
      <div className="m">
        <i className="livedot" aria-hidden="true" />
        {t("meetings.recordingFor", {
          time: formatClock((now - start) / 1000),
        })}
      </div>
    );
  }
  if (status === "transcribing") {
    const percent = progress?.percent ?? null;
    return (
      <div className="m" role="status">
        <ProgressLine
          value={(percent ?? 30) / 100}
          busy={percent === null}
          className="line h-[4px]"
        />
        {percent === null
          ? t("meetings.transcribing")
          : t("meetings.percent", { percent: Math.round(percent) })}
      </div>
    );
  }
  if (status === "failed") {
    return <div className="m err">{t("meetings.failed")}</div>;
  }
  if (hit && q) {
    const i = hit.text.toLocaleLowerCase().indexOf(q.toLocaleLowerCase());
    const from = Math.max(0, i - SNIPPET_LEAD);
    const snip = (from > 0 ? "…" : "") + hit.text.slice(from);
    return (
      <div className="m">
        <span className="snip">
          <Highlight text={snip} query={q} />
        </span>
      </div>
    );
  }
  const date = startedAt(row);
  const sorted = [...(keys ?? [])].sort(speakerOrder);
  return (
    <div className="m">
      {date ? formatTime(i18n.language, date) : null}
      {sorted.length > 0 && (
        <span
          className="dots"
          aria-label={t("meetings.speakerCount", { count: sorted.length })}
        >
          {sorted.map((k) => (
            <i key={k} data-sp={speakerSlot(k)} />
          ))}
        </span>
      )}
    </div>
  );
};

interface MeetingListProps {
  rows: MeetingRow[];
  selectedId: string | null;
  onSelect: (id: string) => void;
}

export const MeetingList: React.FC<MeetingListProps> = ({
  rows,
  selectedId,
  onSelect,
}) => {
  const { t, i18n } = useTranslation();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Record<string, MeetingHit>>({});
  const q = query.trim();

  useEffect(() => {
    if (!q) {
      setHits({});
      return;
    }
    let stale = false;
    const id = setTimeout(async () => {
      const result = await commands.searchMeetings(q);
      if (stale || result.status !== "ok") return;
      setHits(Object.fromEntries(result.data.map((h) => [h.meeting_id, h])));
    }, 150);
    return () => {
      stale = true;
      clearTimeout(id);
    };
  }, [q]);

  const now = new Date();
  const lower = q.toLocaleLowerCase();
  const shown = rows.filter(
    (r) =>
      !q ||
      hits[r.id] ||
      meetingTitle(t, i18n.language, r).toLocaleLowerCase().includes(lower),
  );

  let lastGroup = "";
  return (
    <>
      <label className="field">
        <Search className="ic" aria-hidden="true" />
        <input
          type="search"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={t("meetings.searchAll")}
          aria-label={t("meetings.searchAllLabel")}
        />
      </label>
      <div className="rows">
        {shown.map((row) => {
          const group = dayGroup(i18n.language, row, now);
          const key = dayGroupKey(group);
          const heading = key !== lastGroup;
          lastGroup = key;
          const status = meetingStatus(row);
          const title = meetingTitle(t, i18n.language, row);
          return (
            <React.Fragment key={row.id}>
              {heading && <div className="day">{groupLabel(t, group)}</div>}
              <button
                type="button"
                className="row"
                aria-current={row.id === selectedId}
                onClick={() => onSelect(row.id)}
              >
                <div className="t">
                  <span className="title">
                    <Highlight text={title} query={q} />
                  </span>
                  <span className="dur">
                    {status === "recording"
                      ? ""
                      : formatDuration(t, row.duration_ms)}
                  </span>
                </div>
                <RowMeta row={row} hit={hits[row.id]} q={q} />
              </button>
            </React.Fragment>
          );
        })}
        {q && shown.length === 0 && (
          <div className="list-msg">
            {t("meetings.noMatch", { query: q })}
            <br />
            <Button variant="secondary" size="sm" onClick={() => setQuery("")}>
              {t("meetings.clearSearch")}
            </Button>
          </div>
        )}
      </div>
    </>
  );
};
