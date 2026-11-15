import type { TFunction } from "i18next";
import { platform } from "@tauri-apps/plugin-os";
import { commands, type Notice, type NoticeCode } from "@/bindings";

/** Download id of the speaker diarizer (`DIARIZER_ID` in Rust). */
export const DIARIZER_ID = "diarizer";

interface NoticeDisplay {
  title: string;
  // Without one, the Live overlay shows the notice's detail instead.
  explanation?: (t: TFunction) => string;
  // A button that fixes what the notice reports, when there is one.
  action?: (notice: Notice) => { label: string; run: () => void } | undefined;
}

// See "Error notices" in AGENTS.md for how to add a code.
const NOTICE_DISPLAY: Record<NoticeCode, NoticeDisplay> = {
  TranscriptionFailed: {
    title: "errors.transcriptionFailedTitle",
  },
  PasteFailed: {
    title: "errors.pasteFailedTitle",
    explanation: (t) => t("errors.pasteFailed"),
  },
  PasteBlockedElevated: {
    title: "errors.pasteBlockedElevatedTitle",
    explanation: (t) => t("errors.pasteBlockedElevated"),
  },
  NoMicrophone: {
    title: "errors.noInputDeviceTitle",
    explanation: (t) => t("errors.noInputDevice"),
  },
  MicrophoneSilent: {
    title: "errors.microphoneSilentTitle",
    explanation: (t) => t("errors.microphoneSilent"),
  },
  MicPermissionDenied: {
    title: "errors.micPermissionDeniedTitle",
    explanation: (t) =>
      t(`errors.micPermissionDenied.${platform()}`, {
        defaultValue: t("errors.micPermissionDenied.generic"),
      }),
  },
  RecordingFailed: {
    title: "errors.recordingFailedTitle",
  },
  ModelLoadFailed: {
    title: "errors.modelLoadFailedTitle",
    explanation: (t) => t("errors.modelLoadFailed"),
  },
  NoModelSelected: {
    title: "errors.noModelSelectedTitle",
    explanation: (t) => t("errors.noModelSelected"),
  },
  PostProcessFailed: {
    title: "errors.postProcessFailedTitle",
    explanation: (t) => t("errors.postProcessFailed"),
  },
  FileTranscriptionFailed: {
    title: "errors.fileTranscriptionFailedTitle",
  },
  FilePostProcessFailed: {
    title: "errors.filePostProcessFailedTitle",
    explanation: (t) => t("errors.filePostProcessFailed"),
  },
  MeetingRecordingFailed: {
    title: "errors.meetingRecordingFailedTitle",
  },
  MeetingTranscriptionFailed: {
    title: "errors.meetingTranscriptionFailedTitle",
  },
  MeetingSpeakersNotSeparated: {
    title: "errors.meetingSpeakersNotSeparatedTitle",
    // With a detail the diarizer failed; without one it isn't downloaded.
    action: (notice) =>
      notice.detail
        ? undefined
        : {
            label: "errors.downloadSpeakerModel",
            run: () => void commands.downloadDiarizer(),
          },
  },
  MeetingNoteFailed: {
    title: "errors.meetingNoteFailedTitle",
  },
};

export const noticeDisplay = (code: NoticeCode) => NOTICE_DISPLAY[code];
