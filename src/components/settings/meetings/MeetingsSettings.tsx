import React, { useEffect, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { Button } from "@/components/ui/Button";
import { commands } from "@/bindings";
import { DetailSkeleton, MeetingDetail } from "./MeetingDetail";
import { MeetingList, StartStopButton } from "./MeetingList";
import { useMeetingsStore } from "./meetingsStore";
import "./meetings.css";

const Empty: React.FC = () => {
  const { t } = useTranslation();
  return (
    <div className="empty">
      <div className="ghost-lanes" aria-hidden="true">
        <span>
          <b style={{ insetInlineStart: "4%", width: "22%" }} />
          <b style={{ insetInlineStart: "52%", width: "14%" }} />
          <b style={{ insetInlineStart: "84%", width: "9%" }} />
        </span>
        <span>
          <b style={{ insetInlineStart: "28%", width: "21%" }} />
          <b style={{ insetInlineStart: "70%", width: "11%" }} />
        </span>
        <span>
          <b style={{ insetInlineStart: "14%", width: "8%" }} />
          <b style={{ insetInlineStart: "44%", width: "6%" }} />
        </span>
      </div>
      <h3>{t("meetings.emptyTitle")}</h3>
      <p>{t("meetings.emptyBody")}</p>
      <p>{t("meetings.emptyModel")}</p>
      <StartStopButton size="md" />
      <small>
        <Trans i18nKey="meetings.emptyHint" components={{ kbd: <kbd /> }} />
      </small>
    </div>
  );
};

const LoadError: React.FC<{ error: string }> = ({ error }) => {
  const { t } = useTranslation();
  const refresh = useMeetingsStore((s) => s.refresh);
  return (
    <div className="failbox" role="alert">
      <h3>{t("meetings.loadFailedTitle")}</h3>
      <p>{t("meetings.loadFailedBody", { error })}</p>
      <div className="rowbtns">
        <Button variant="secondary" size="md" onClick={() => refresh()}>
          {t("meetings.tryAgain")}
        </Button>
        <Button
          variant="secondary"
          size="md"
          onClick={() => commands.openRecordingsFolder()}
        >
          {t("meetings.openRecordings")}
        </Button>
      </div>
    </div>
  );
};

/** Meetings: the list beside the open meeting, or one of them when narrow. */
export const MeetingsSettings: React.FC = () => {
  const { t } = useTranslation();
  const rows = useMeetingsStore((s) => s.rows);
  const error = useMeetingsStore((s) => s.error);
  const recordingId = useMeetingsStore((s) => s.recordingId);
  const [selected, setSelected] = useState<string | null>(null);
  const [view, setView] = useState<"list" | "detail">("list");

  // Jump to a meeting that starts recording.
  useEffect(() => {
    if (recordingId) setSelected(recordingId);
  }, [recordingId]);

  // The newest meeting until one is picked.
  const row = rows?.find((r) => r.id === selected) ?? rows?.[0] ?? null;
  const solo = rows !== null && rows.length === 0 && !error;

  return (
    <section
      className={`mtg ${solo ? "solo" : ""}`}
      data-view={view}
      aria-label={t("meetings.title")}
    >
      <div className="list">
        <div className="list-head">
          <h2 className="label">{t("meetings.title")}</h2>
          <StartStopButton />
        </div>
        {error && !rows ? (
          <div className="list-msg" role="alert">
            {t("meetings.loadFailed")}
          </div>
        ) : rows === null ? (
          <div className="rows" aria-busy="true">
            {[72, 58, 80, 46, 66, 54].map((w, i) => (
              <div key={w} className="skelrow">
                <span className="skel" style={{ height: 12, width: `${w}%` }} />
                <span
                  className="skel"
                  style={{
                    height: 9,
                    width: `${[34, 40, 28, 36, 30, 42][i]}%`,
                  }}
                />
              </div>
            ))}
          </div>
        ) : (
          <MeetingList
            rows={rows}
            selectedId={row?.id ?? null}
            onSelect={(id) => {
              setSelected(id);
              setView("detail");
            }}
          />
        )}
      </div>
      <div className="detail">
        {error && !rows ? (
          <LoadError error={error} />
        ) : solo ? (
          <Empty />
        ) : row ? (
          <MeetingDetail
            key={row.id}
            row={row}
            onBack={() => setView("list")}
          />
        ) : (
          <DetailSkeleton />
        )}
      </div>
    </section>
  );
};
