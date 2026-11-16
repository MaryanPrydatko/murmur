import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { commands, type MeetingRow } from "@/bindings";
import { Button } from "@/components/ui/Button";
import { Dialog } from "@/components/ui/Dialog";
import { notify } from "@/stores/noticeStore";

interface DeleteMeetingDialogProps {
  /** The meeting to delete; null keeps the dialog closed. */
  row: MeetingRow | null;
  onClose: (deleted: boolean) => void;
}

/** Asks before a meeting's recordings and transcript are removed for good. */
export const DeleteMeetingDialog: React.FC<DeleteMeetingDialogProps> = ({
  row,
  onClose,
}) => {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);

  const confirm = async () => {
    if (!row || busy) return;
    setBusy(true);
    const result = await commands.deleteMeeting(row.id);
    setBusy(false);
    if (result.status === "error")
      notify({ title: t("meetings.deleteFailed"), detail: result.error });
    onClose(result.status === "ok");
  };

  return (
    <Dialog
      open={row !== null}
      title={t("meetings.deleteTitle")}
      closeLabel={t("common.close")}
      onOpenChange={() => onClose(false)}
      dismissible={!busy}
      contentFades={false}
      footer={
        <>
          <Button
            variant="secondary"
            disabled={busy}
            onClick={() => onClose(false)}
          >
            {t("meetings.deleteCancel")}
          </Button>
          <Button variant="danger" disabled={busy} onClick={confirm}>
            {t("meetings.delete")}
          </Button>
        </>
      }
    >
      <p className="text-sm leading-relaxed text-text">
        {t("meetings.deleteBody")}
      </p>
      <p className="mt-2 text-sm leading-relaxed text-ink-3 break-words">
        {row?.note_path
          ? t("meetings.deleteNoteStays", { path: row.note_path })
          : t("meetings.deleteNoteMayStay")}
      </p>
    </Dialog>
  );
};
