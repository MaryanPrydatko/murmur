import React from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { SettingContainer } from "../ui/SettingContainer";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface MeetingNotesProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

// Where a transcribed meeting's note goes: a folder in an Obsidian vault.
// No vault, no notes. The inputs stay enabled while a keystroke saves, or
// they'd lose focus after every character.
export const MeetingNotes: React.FC<MeetingNotesProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const vault = getSetting("obsidian_vault_path") ?? "";
    const folder = getSetting("obsidian_notes_folder") ?? "";
    const summarise = getSetting("summarise_meetings") ?? false;

    const browse = async () => {
      const picked = await open({ directory: true, multiple: false });
      if (typeof picked === "string") {
        updateSetting("obsidian_vault_path", picked);
      }
    };

    return (
      <>
        <SettingContainer
          title={t("settings.advanced.meetingNotes.vault.title")}
          description={t("settings.advanced.meetingNotes.vault.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <div className="flex items-center gap-2">
            <Input
              type="text"
              value={vault}
              onChange={(e) =>
                updateSetting("obsidian_vault_path", e.target.value || null)
              }
              placeholder={t(
                "settings.advanced.meetingNotes.vault.placeholder",
              )}
            />
            <Button variant="secondary" size="sm" onClick={browse}>
              {t("settings.advanced.meetingNotes.vault.browse")}
            </Button>
          </div>
        </SettingContainer>
        <SettingContainer
          title={t("settings.advanced.meetingNotes.folder.title")}
          description={t("settings.advanced.meetingNotes.folder.description")}
          descriptionMode={descriptionMode}
          grouped={grouped}
        >
          <Input
            type="text"
            value={folder}
            onChange={(e) =>
              updateSetting("obsidian_notes_folder", e.target.value)
            }
            placeholder={t("settings.advanced.meetingNotes.folder.placeholder")}
            disabled={!vault}
          />
        </SettingContainer>
        <ToggleSwitch
          checked={summarise}
          onChange={(enabled) => updateSetting("summarise_meetings", enabled)}
          isUpdating={isUpdating("summarise_meetings")}
          label={t("settings.advanced.meetingNotes.summarise.title")}
          description={t(
            "settings.advanced.meetingNotes.summarise.description",
          )}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />
      </>
    );
  },
);
