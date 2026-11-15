import React from "react";
import { useTranslation } from "react-i18next";
import {
  AudioLines,
  Cpu,
  FlaskConical,
  History,
  Info,
  Mic,
  SlidersHorizontal,
  Sparkles,
} from "lucide-react";
import Wordmark from "./icons/Wordmark";
import { useSettings } from "../hooks/useSettings";
import {
  GeneralSettings,
  AdvancedSettings,
  HistorySettings,
  AboutSettings,
  PostProcessingSettings,
  ModelsSettings,
} from "./settings";
import { MeetingsSettings } from "./settings/meetings/MeetingsSettings";
import { useMeetingsStore } from "./settings/meetings/meetingsStore";

export type SidebarSection = keyof typeof SECTIONS_CONFIG;

interface IconProps {
  width?: number | string;
  height?: number | string;
  size?: number | string;
  className?: string;
  [key: string]: any;
}

interface SectionConfig {
  labelKey: string;
  icon: React.ComponentType<IconProps>;
  /** `null` when App renders the section itself (Debug, with its handlers). */
  component: React.ComponentType | null;
  enabled: (settings: any) => boolean;
  /** Content sections come first, then a hairline, then settings. */
  group: "content" | "settings";
}

export const SECTIONS_CONFIG = {
  meetings: {
    labelKey: "sidebar.meetings",
    icon: AudioLines,
    component: MeetingsSettings,
    enabled: () => true,
    group: "content",
  },
  history: {
    labelKey: "sidebar.history",
    icon: History,
    component: HistorySettings,
    enabled: () => true,
    group: "content",
  },
  general: {
    labelKey: "sidebar.general",
    icon: Mic,
    component: GeneralSettings,
    enabled: () => true,
    group: "settings",
  },
  models: {
    labelKey: "sidebar.models",
    icon: Cpu,
    component: ModelsSettings,
    enabled: () => true,
    group: "settings",
  },
  advanced: {
    labelKey: "sidebar.advanced",
    icon: SlidersHorizontal,
    component: AdvancedSettings,
    enabled: () => true,
    group: "settings",
  },
  postprocessing: {
    labelKey: "sidebar.postProcessing",
    icon: Sparkles,
    component: PostProcessingSettings,
    enabled: (settings) => settings?.post_process_enabled ?? false,
    group: "settings",
  },
  debug: {
    labelKey: "sidebar.debug",
    icon: FlaskConical,
    component: null,
    enabled: (settings) => settings?.debug_mode ?? false,
    group: "settings",
  },
  about: {
    labelKey: "sidebar.about",
    icon: Info,
    component: AboutSettings,
    enabled: () => true,
    group: "settings",
  },
} as const satisfies Record<string, SectionConfig>;

interface SidebarProps {
  activeSection: SidebarSection;
  onSectionChange: (section: SidebarSection) => void;
}

export const Sidebar: React.FC<SidebarProps> = ({
  activeSection,
  onSectionChange,
}) => {
  const { t } = useTranslation();
  const { settings } = useSettings();

  const meetingRecording = useMeetingsStore((s) => s.recordingId !== null);

  const availableSections = Object.entries(SECTIONS_CONFIG)
    .filter(([_, config]) => config.enabled(settings))
    .map(([id, config]) => ({ id: id as SidebarSection, ...config }));

  return (
    <nav
      aria-label={t("sidebar.label")}
      className="flex flex-col w-40 h-full border-e border-line items-center px-2"
    >
      <Wordmark width={124} className="my-4" />
      <div className="flex flex-col w-full items-center gap-1 pt-2 border-t border-line">
        {availableSections.map((section, i) => {
          const Icon = section.icon;
          const isActive = activeSection === section.id;
          const startsGroup =
            i > 0 && availableSections[i - 1].group !== section.group;

          return (
            <React.Fragment key={section.id}>
              {startsGroup && <hr className="w-full my-1 border-line" />}
              <button
                type="button"
                aria-current={isActive ? "page" : undefined}
                className={`sidebar-item flex gap-2 items-center p-2 w-full rounded-lg cursor-pointer transition-colors text-start ${
                  isActive ? "" : "hover:bg-line hover:opacity-100 opacity-85"
                }`}
                onClick={() => onSectionChange(section.id)}
              >
                <Icon
                  width={24}
                  height={24}
                  strokeWidth={1.75}
                  className="shrink-0"
                />
                <span
                  className="text-[13px] leading-5 font-medium truncate"
                  title={t(section.labelKey)}
                >
                  {t(section.labelKey)}
                </span>
                {section.id === "meetings" && meetingRecording && (
                  <i
                    className="ms-auto size-2 shrink-0 rounded-full bg-[var(--color-rec)] animate-pulse motion-reduce:animate-none"
                    aria-label={t("meetings.recording")}
                  />
                )}
              </button>
            </React.Fragment>
          );
        })}
      </div>
    </nav>
  );
};
