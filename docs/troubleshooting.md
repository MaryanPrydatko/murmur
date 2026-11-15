# Troubleshooting and platform notes

Technical notes carried over from Handy, which Murmur is built on. The binary is still called `handy` and the environment variables still start with `HANDY_`. Issue numbers link to Handy's tracker, where the bugs were found.

## Transcription hook

Advanced users on macOS and Linux can rewrite text right before Murmur pastes it. Put an executable at `hooks/transcription` in Murmur's app data folder (`~/Library/Application Support/com.maryan.murmur/` on macOS, `~/.local/share/com.maryan.murmur/` on Linux). Murmur writes the text to its stdin, after any LLM post-processing, and pastes its stdout. One trailing newline is removed, and empty output pastes nothing. If the hook fails or runs longer than 30 seconds, the original text is pasted.

The hook doesn't get your shell's `PATH`: on macOS, Murmur started from the Dock or Finder only sees `/usr/bin:/bin:/usr/sbin:/sbin`. Use absolute paths for anything else, such as `/opt/homebrew/bin/opencc`.

Example that prefixes `#` when Terminal is the frontmost app (macOS):

```bash
mkdir -p ~/Library/Application\ Support/com.maryan.murmur/hooks
cd ~/Library/Application\ Support/com.maryan.murmur/hooks
cat > transcription << 'EOF'
#!/bin/bash
app=$(osascript -e 'tell application "System Events" to get name of first application process whose frontmost is true')
if [ "$app" = "Terminal" ]; then echo "# $(cat)"; else cat; fi
EOF
chmod +x transcription
```

## macOS

### Bluetooth Headset Microphones (macOS)

Using a Bluetooth headset microphone on macOS may temporarily reduce playback quality or volume while recording because Bluetooth switches to bidirectional audio. Keep your headphones as the output device and select your Mac's built-in or an external microphone in Murmur to avoid this.

### fn and Globe Key Shortcuts (macOS)

Shortcuts that include the `fn` (Globe) key **only work on Apple keyboards**, your Mac's built-in keyboard or an Apple external keyboard. They will never trigger on a third-party keyboard, even while it is connected to the same Mac.

This is a hardware limitation rather than a Murmur bug. `fn` is not part of the standard USB HID keyboard specification: Apple reports it through a vendor-specific usage that macOS honors only from Apple devices, while third-party keyboards handle their `Fn` key entirely in firmware and send nothing to the computer. There is no event for Murmur to listen for.

If you switch between a MacBook keyboard and an external one, pick a shortcut built from standard modifiers (`ctrl`, `option`, `shift`, `command`) or a regular key instead.

## Linux notes

**Paste Method:**

On Linux, new installs paste with **Shift+Insert** (**Clipboard (Shift+Insert)** in **Advanced → Paste Method**). It puts the transcript on both the clipboard and the primary selection, so the same key pastes in GUI apps (which read the clipboard) and in terminals such as kitty, Ghostty, Alacritty and GNOME Terminal (which read the primary selection), then restores both. **Direct** typing is still available, but it depends on your keyboard layout: it can drop capitals and accented characters or type the wrong characters on non-US layouts.

**Text Input Tools:**

Murmur needs a tool to send the paste key (or to type, with Direct). Install the appropriate one for your display server:

| Display Server                | Recommended Tool | Install Command                                    |
| ----------------------------- | ---------------- | -------------------------------------------------- |
| X11                           | `xdotool`        | `sudo apt install xdotool`                         |
| Wayland, except GNOME and KDE | `wtype`          | `sudo apt install wtype`                           |
| Wayland on GNOME or KDE       | `ydotool`        | `sudo apt install ydotool` (needs its service)     |
| Both                          | `dotool`         | `sudo apt install dotool` (requires `input` group) |

- **X11**: Install `xdotool` for both direct typing and clipboard paste shortcuts
- **Ubuntu 26.04**: Has Wayland display server by default. `wtype` does not work, you need to install `ydotool` and configure systemd as described [here](https://github.com/cjpais/Handy/pull/557#issuecomment-3781249267).
- **Wayland, except GNOME and KDE**: Install `wtype` (preferred) or `dotool` for text input to work correctly
- **GNOME and KDE on Wayland**: Murmur does not use `wtype` there, because these desktops lack the virtual keyboard protocol it needs. Install `dotool` or `ydotool`. On KDE, `kwtype` is used for Direct typing when it is installed.
- **dotool setup**: Requires adding your user to the `input` group: `sudo usermod -aG input $USER` (then log out and back in)

Without these tools, Murmur falls back to enigo which may have limited compatibility, especially on Wayland.

**Wayland Support (Linux):**

- Limited support for Wayland display server
- Requires [`wtype`](https://github.com/atx/wtype), [`dotool`](https://sr.ht/~geb/dotool/) or `ydotool` for text input to work correctly (see above for which one your desktop needs)

**Other Notes:**

- **Runtime library dependency (`libgtk-layer-shell.so.0`)**:
  - Murmur links `gtk-layer-shell` on Linux. If startup fails with `error while loading shared libraries: libgtk-layer-shell.so.0`, install the runtime package for your distro:

    | Distro        | Package to install    | Example command                        |
    | ------------- | --------------------- | -------------------------------------- |
    | Ubuntu/Debian | `libgtk-layer-shell0` | `sudo apt install libgtk-layer-shell0` |
    | Fedora/RHEL   | `gtk-layer-shell`     | `sudo dnf install gtk-layer-shell`     |
    | Arch Linux    | `gtk-layer-shell`     | `sudo pacman -S gtk-layer-shell`       |

  - For building from source on Ubuntu/Debian, you may also need `libgtk-layer-shell-dev`.

- The recording overlay is disabled by default on Linux (`Overlay Position: None`) because certain compositors treat it as the active window. When the overlay is visible it can steal focus, which prevents Murmur from pasting back into the application that triggered transcription. If you enable the overlay anyway, be aware that clipboard-based pasting might fail or end up in the wrong window.
- Murmur picks the WebKit renderer on Linux from the linked WebKitGTK version: 2.54 and newer keep the compositor on shared memory (`WEBKIT_DMABUF_RENDERER_FORCE_SHM=1`), older releases disable DMA-BUF rendering (`WEBKIT_DISABLE_DMABUF_RENDERER=1`). Explicit values for either variable are preserved. For rendering workarounds, see [Troubleshooting → Configure the WebKit renderer](#3-configure-the-webkit-renderer).
- If Murmur fails to start reliably on Linux, see [Troubleshooting → Linux Startup Crashes or Instability](#linux-startup-crashes-or-instability).
- **Global keyboard shortcuts (Wayland):** On Wayland, system-level shortcuts must be configured through your desktop environment or window manager. Use the [CLI flags](../README.md#command-line) as the command for your custom shortcut.

  **GNOME:**
  1. Open **Settings > Keyboard > Keyboard Shortcuts > Custom Shortcuts**
  2. Click the **+** button to add a new shortcut
  3. Set the **Name** to `Toggle Murmur Transcription`
  4. Set the **Command** to `handy --toggle-transcription`
  5. Click **Set Shortcut** and press your desired key combination (e.g., `Super+O`)

  **KDE Plasma:**
  1. Open **System Settings > Shortcuts > Custom Shortcuts**
  2. Click **Edit > New > Global Shortcut > Command/URL**
  3. Name it `Toggle Murmur Transcription`
  4. In the **Trigger** tab, set your desired key combination
  5. In the **Action** tab, set the command to `handy --toggle-transcription`

  **Sway / i3:**

  Add to your config file (`~/.config/sway/config` or `~/.config/i3/config`):

  ```ini
  bindsym $mod+o exec handy --toggle-transcription
  ```

  **Hyprland:**

  Add to your config file (`~/.config/hypr/hyprland.conf`):

  ```ini
  bind = $mainMod, O, exec, handy --toggle-transcription
  ```

- You can also trigger Murmur externally via Unix signals or the CLI flags, which lets Wayland window managers or other hotkey daemons keep ownership of keybindings:

  | Action                                    | Trigger                                                  |
  | ----------------------------------------- | -------------------------------------------------------- |
  | Toggle transcription                      | `pkill -USR2 -n handy` or `handy --toggle-transcription` |
  | Toggle transcription with post-processing | `handy --toggle-post-process`                            |

  Example Sway config:

  ```ini
  bindsym $mod+o exec pkill -USR2 -n handy
  bindsym $mod+p exec handy --toggle-post-process
  ```

  `pkill` here simply delivers the signal. It does not terminate the process.

  > **Behavior change:** older releases also accepted `SIGUSR1` for toggling transcription with post-processing. WebKitGTK, the webview engine embedded in Murmur on Linux, uses SIGUSR1 internally to coordinate JavaScript garbage collection, so listening for it caused phantom recordings and interrupted dictations every few minutes ([#1660](https://github.com/cjpais/Handy/issues/1660)). Murmur no longer listens for SIGUSR1 on Linux; the post-processing toggle is still available via `handy --toggle-post-process`. **Remove any `pkill -USR1` bindings**: the signal is now delivered straight to WebKit's internal handler and can crash the app.

**Overlay & Pasting Issues (Linux):**

- The recording overlay window can interfere with pasting transcribed text into target applications on Linux (X11)
- **Solution:** Open **Settings > Advanced** and set **"Overlay Position"** to **"None"** to disable the overlay
- Enable **"Audio Feedback"** (also in Advanced) if you still want audible confirmation of recording state
- Users who upgrade from older versions or import settings from other platforms may need to manually apply this change

## Troubleshooting

### Previous Clipboard Content Is Pasted Instead of the Transcription

If the transcription is correct in **History** but Murmur inserts text you copied earlier, see [issue #502](https://github.com/cjpais/Handy/issues/502). With the standard clipboard paste method, Murmur restores your previous clipboard after a fixed delay. Under load, the receiving application may read the clipboard only after that restoration.

1. Open Murmur's settings window and press `Cmd+Shift+D` (macOS) or `Ctrl+Shift+D` (Windows/Linux) to reveal **Debug**.
2. On **macOS and Windows**, try **Reliable Paste (Beta)** in Debug with a clipboard paste method selected. It uses clipboard read notifications to delay restoration instead of relying on the standard fixed delay. Test it in the application where the problem occurs; it is still experimental.
3. If Reliable Paste is disabled or unavailable, increase **Paste Delay (After)** in Debug and test again. This controls the wait before restoring your previous clipboard. **Paste Delay (Before)** controls the wait before sending the paste keystroke and addresses a different part of the operation. These delay settings apply to the standard paste path, not Reliable Paste.

If the problem persists, add your Murmur version, operating system, receiving application, paste method, Reliable Paste setting, and before/after delays to the existing issue. Redact private dictated text before sharing logs.

### Manual Model Installation (For Proxy Users or Network Restrictions)

If you're behind a proxy, firewall, or in a restricted network environment where Murmur cannot download models automatically, you can manually download and install them. The URLs are publicly accessible from any browser.

#### Step 1: Find Your App Data Directory

1. Open Murmur settings
2. Navigate to the **About** section
3. Copy the "App Data Directory" path shown there, or use the shortcuts:
   - **macOS**: `Cmd+Shift+D` to open debug menu
   - **Windows/Linux**: `Ctrl+Shift+D` to open debug menu

The typical paths are:

- **macOS**: `~/Library/Application Support/com.maryan.murmur/`
- **Linux**: `~/.local/share/com.maryan.murmur/`

#### Step 2: Create Models Directory

Inside your app data directory, create a `models` folder if it doesn't already exist:

```bash
# macOS
mkdir -p ~/Library/Application\ Support/com.maryan.murmur/models

# Linux
mkdir -p ~/.local/share/com.maryan.murmur/models
```

#### Step 3: Download Model Files

Download the models you want from below

**Whisper Models (single .bin files):**

- Small (487 MB): `https://blob.handy.computer/ggml-small.bin`
- Medium (492 MB): `https://blob.handy.computer/whisper-medium-q4_1.bin`
- Turbo (1600 MB): `https://blob.handy.computer/ggml-large-v3-turbo.bin`
- Large (1100 MB): `https://blob.handy.computer/ggml-large-v3-q5_0.bin`

**Parakeet Unified EN 0.6B (single `.gguf` file, recommended):**

- Q8_0 (731 MB): `https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/main/parakeet-unified-en-0.6b-Q8_0.gguf`

#### Step 4: Install Models

**For Whisper Models (.bin files):**

Simply place the `.bin` file directly into the `models` directory:

```
{app_data_dir}/models/
├── ggml-small.bin
├── whisper-medium-q4_1.bin
├── ggml-large-v3-turbo.bin
└── ggml-large-v3-q5_0.bin
```

**For GGUF Models (.gguf files):**

Place the `.gguf` file directly into the `models` directory, exactly like the Whisper `.bin` files above. Murmur also picks up models already present in the shared Hugging Face cache (`~/.cache/huggingface/hub`), so a copy downloaded by another tool works without being moved.

**Important Notes:**

- Do not rename the `.bin` or `.gguf` files: use the exact filenames from the download URLs
- After placing the files, restart Murmur to detect the new models

#### Step 5: Verify Installation

1. Restart Murmur
2. Open Settings → Models
3. Your manually installed models should now appear as "Downloaded"
4. Select the model you want to use and test transcription

### Custom Whisper Models

Murmur can auto-discover custom Whisper GGML models placed in the `models` directory. This is useful for users who want to use fine-tuned or community models not included in the default model list.

**How to use:**

1. Obtain a Whisper model in GGML `.bin` format (e.g., from [Hugging Face](https://huggingface.co/models?search=whisper%20ggml))
2. Place the `.bin` file in your `models` directory (see paths above)
3. Restart Murmur to discover the new model
4. The model will appear in the "Custom Models" section of the Models settings page

**Important:**

- Community models are user-provided and may not receive troubleshooting assistance
- The model must be a valid Whisper GGML format (`.bin` file)
- Model name is derived from the filename (e.g., `my-custom-model.bin` → "My Custom Model")

### Linux Startup Crashes or Instability

If Murmur fails to start reliably on Linux, for example, it crashes shortly after launch, never shows its window, or reports a Wayland protocol error, try the steps below in order.

**1. Install (or reinstall) `gtk-layer-shell`**

Murmur uses `gtk-layer-shell` for its recording overlay and links against it at runtime. A missing or broken installation is the most common cause of startup failures and can manifest as a crash or a hang well before any window is shown. Make sure the runtime package is installed for your distro:

| Distro        | Package to install    | Example command                        |
| ------------- | --------------------- | -------------------------------------- |
| Ubuntu/Debian | `libgtk-layer-shell0` | `sudo apt install libgtk-layer-shell0` |
| Fedora/RHEL   | `gtk-layer-shell`     | `sudo dnf install gtk-layer-shell`     |
| Arch Linux    | `gtk-layer-shell`     | `sudo pacman -S gtk-layer-shell`       |

If it is already installed and you still see startup problems, try reinstalling it (e.g. `sudo pacman -S gtk-layer-shell` again) in case the library files were corrupted by a partial upgrade.

**2. Disable the GTK layer shell overlay (`HANDY_NO_GTK_LAYER_SHELL`)**

If installing the library does not help, you can skip `gtk-layer-shell` initialization entirely as a workaround. On some compositors (notably KDE Plasma under Wayland) it has been reported to interact poorly with the recording overlay. With this variable set, the overlay falls back to a regular always-on-top window:

```bash
HANDY_NO_GTK_LAYER_SHELL=1 handy
```

**3. Configure the WebKit renderer (`WEBKIT_DISABLE_DMABUF_RENDERER` / `WEBKIT_DMABUF_RENDERER_FORCE_SHM`)**

Murmur picks the renderer at startup from the WebKitGTK build it links, because the DMA-BUF renderer crashes on some GPU/driver combinations but the non-composited fallback loses pixels on the 2.54 series:

- **WebKitGTK 2.54 and newer:** the compositor stays enabled and is forced onto shared memory with `WEBKIT_DMABUF_RENDERER_FORCE_SHM=1`. 2.54 composites only the damaged region of every frame, so the previous workaround (`WEBKIT_DISABLE_DMABUF_RENDERER=1`) left transparent windows, such as the recording overlay, showing only their animated parts ([#2166](https://github.com/cjpais/Handy/issues/2166)).
- **Older releases:** DMA-BUF rendering is disabled with `WEBKIT_DISABLE_DMABUF_RENDERER=1`, which can cause blank windows but matches the previous behavior.

Explicit environment-variable values are preserved, including in the Nix package:

```bash
# Force the legacy non-composited renderer (2.54: partial frames may reappear)
WEBKIT_DISABLE_DMABUF_RENDERER=1 handy

# Keep the compositor on shared memory (the 2.54 default below)
WEBKIT_DMABUF_RENDERER_FORCE_SHM=1 handy

# Let WebKit use DMA-BUF when supported (may reintroduce the GPU/driver crashes)
WEBKIT_DISABLE_DMABUF_RENDERER=0 handy
```

Fully quit Murmur, including its tray instance, before testing a value. Running a second instance with `--toggle-transcription` does not change the running process's environment.

**Making a workaround permanent**

Set the variable in the environment of the launcher that actually starts Murmur. Shell profile exports may not apply to desktop-menu or autostart launches. If you launch Murmur from a `.desktop` file, you can prefix the `Exec=` line, e.g.:

```ini
Exec=env HANDY_NO_GTK_LAYER_SHELL=1 handy
```

Murmur's built-in autostart rewrites its generated desktop entry, so editing that entry is not a durable workaround. For autostart, use an appropriate session/service environment, or disable Murmur's built-in autostart and manage your own startup entry with the required variable.

If a workaround helps you, please [open an issue](https://github.com/MaryanPrydatko/murmur/issues) describing your distro, desktop environment, and session type. That information helps narrow down the underlying bug.

### Empty recording overlay on Hyprland / Omarchy

If the recording overlay is empty or bordered, fully quit Murmur and launch a
[native installation](../BUILD.md#linux-install-from-source) with Wayland enabled:

```bash
env -u HANDY_NO_GTK_LAYER_SHELL GDK_BACKEND=wayland handy
```

If this works, apply `GDK_BACKEND=wayland` only to Murmur's launcher.

The AppImage forces `GDK_BACKEND=x11` in its launch script, after your environment, so
`GDK_BACKEND=wayland ./Murmur.AppImage` has no effect. Use `HANDY_GDK_BACKEND` instead; Murmur copies it
into `GDK_BACKEND` before GTK starts:

```bash
HANDY_GDK_BACKEND=wayland ./Murmur_x.y.z_amd64.AppImage
```

For a launcher, put the variable in the `.desktop` entry, for example
`Exec=env HANDY_GDK_BACKEND=wayland /path/to/Murmur_x.y.z_amd64.AppImage`.

Try native Wayland if the AppImage shows a frozen Language dropdown, a black window after closing
to the tray and relaunching ([#1729](https://github.com/cjpais/Handy/issues/1729)), or exits with
`Failed to initialize gtk backend` on a Wayland-only compositor such as Niri without XWayland
([#2034](https://github.com/cjpais/Handy/issues/2034)). The AppImage defaults to X11 because of
[tauri#8541](https://github.com/tauri-apps/tauri/issues/8541), so this stays opt-in.
