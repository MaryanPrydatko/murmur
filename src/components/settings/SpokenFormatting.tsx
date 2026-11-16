import React from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { useSettings } from "../../hooks/useSettings";

interface SpokenFormattingProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const SpokenFormatting: React.FC<SpokenFormattingProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();
    const enabled = getSetting("spoken_formatting_enabled") ?? true;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(nextEnabled) =>
          updateSetting("spoken_formatting_enabled", nextEnabled)
        }
        isUpdating={isUpdating("spoken_formatting_enabled")}
        label={t("settings.advanced.spokenFormatting.title")}
        description={t("settings.advanced.spokenFormatting.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);
