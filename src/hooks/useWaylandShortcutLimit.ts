import { useEffect, useState } from "react";
import { commands } from "@/bindings";
import { useSettings } from "@/hooks/useSettings";

/** Asked once: the session does not change while the app runs. */
let session: Promise<boolean> | undefined;
const isWaylandSession = () =>
  (session ??= commands.isWaylandSession().catch(() => false));

/**
 * Whether the shortcut only works in some windows. On a Wayland session the
 * Tauri implementation grabs the keys through X11, and that grab does not
 * see keys pressed in a native Wayland window.
 */
export const useWaylandShortcutLimit = (): boolean => {
  const { getSetting } = useSettings();
  const [wayland, setWayland] = useState(false);
  useEffect(() => {
    let live = true;
    isWaylandSession().then((is) => {
      if (live) setWayland(is);
    });
    return () => {
      live = false;
    };
  }, []);
  return wayland && getSetting("keyboard_implementation") === "tauri";
};
