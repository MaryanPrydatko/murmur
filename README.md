# Murmur

Local dictation and meeting notes for Linux and macOS.

Press a shortcut, speak, and the text is typed into the app you are in. Start a meeting and Murmur records it, writes down who said what, and puts a note in your Obsidian vault. Everything runs on your machine. Audio and text are not uploaded.

Murmur is a fork of [Handy](https://github.com/cjpais/Handy) by CJ Pais.

## What it does

- **Dictation.** Hold or tap a global shortcut, talk, and the transcript is pasted where your cursor is.
- **Meetings.** Records your microphone and the computer's audio as two tracks, then transcribes both. Computer audio works on Linux today, see [macOS](#macos).
- **Speaker labels.** Your side is labelled as you. The other side is split into speakers, which you can rename. Splitting needs a speaker model that is not installed at first: until you download it, the other side stays one speaker and a notice after the meeting offers the download.
- **Notes to Obsidian.** Set a vault in Settings and each finished meeting is written there as a note, in the `Meetings` folder by default. A note is written once and never overwritten.
- **Local models.** Models are downloaded in the app and run on your CPU or GPU. Parakeet Ultra covers 25 European languages. Whisper and other models can be picked in Settings > Models.

## Install

Downloads are on the [Releases page](https://github.com/MaryanPrydatko/murmur/releases).

### Linux

Debian and Ubuntu: download the `.deb` and install it with APT, so its dependencies come with it:

```bash
sudo apt install ./Murmur_*_amd64.deb
```

Other distributions: download the `.AppImage`, make it executable and run it:

```bash
chmod +x Murmur_*_amd64.AppImage
./Murmur_*_amd64.AppImage
```

The `.deb` adds Murmur to the app grid. The AppImage is a single file you start yourself. Either way Murmur sits in the system tray while it runs.

The `.deb` installs the same files as Handy's `.deb` (the `handy` command among them), so the two cannot be installed side by side: APT removes Handy's package when it installs Murmur's, and asks first.

Pasting needs a helper tool that the packages do not install. Which one depends on your desktop, see [Linux notes](docs/troubleshooting.md#linux-notes).

### macOS

A release that has a macOS build ships it as a `.dmg`. Until the first one is out there is nothing to download for macOS, and you can [build from source](#build-from-source).

The `.dmg` is not signed with an Apple developer certificate, so macOS blocks the first launch. To allow it once:

1. Open the `.dmg` and drag Murmur to Applications.
2. Open Murmur. macOS says it cannot be opened. Close that dialog.
3. Go to System Settings > Privacy & Security, scroll down to the message about Murmur and click **Open Anyway**.
4. Confirm with your password. After that Murmur opens like any other app.

Murmur asks for microphone and accessibility access on first run. Recording the computer's audio in a meeting is coming with the first macOS build.

## Use

### Dictation

The default shortcut is `Ctrl+Alt+Space` on Linux and `Option+Space` on macOS. Hold it while you talk and let go to finish, or tap it once to start and once to stop. You can change it in Settings. On macOS `Esc` cancels. On Linux there is no cancel key: cancel from the tray menu or with `handy --cancel`.

On Wayland the desktop may not pass global shortcuts to apps. In that case bind a system shortcut to `handy --toggle-transcription` (see [Command line](#command-line) and [Linux notes](docs/troubleshooting.md#linux-notes)).

### Meetings

Start and stop a meeting from the tray menu (**Start Meeting** / **Stop Meeting**), from the Meetings page in the app, or from a terminal:

```bash
handy --toggle-meeting
```

When you stop, the recording is transcribed and shows up on the Meetings page, where you can play it, search it, rename speakers and open the note. You can keep dictating while a meeting is recording.

### Command line

The `.deb` installs the command as `handy`, the name it has upstream. With the AppImage there is no installed command: run the AppImage file with the same flags. These flags are sent to the running app:

```bash
handy --toggle-transcription    # start or stop dictation
handy --toggle-post-process     # start or stop dictation with post-processing
handy --toggle-meeting          # start or stop recording a meeting
handy --cancel                  # cancel the current operation
```

Startup flags:

```bash
handy --start-hidden            # start without showing the window
handy --no-tray                 # start without the tray icon
handy --debug                   # verbose logging
handy --help                    # all flags
```

## Where your data lives

| System | Folder                                             |
| ------ | -------------------------------------------------- |
| Linux  | `~/.local/share/com.maryan.murmur/`                |
| macOS  | `~/Library/Application Support/com.maryan.murmur/` |

In that folder:

- `settings_store.json`: settings
- `history.db`: dictation history and meetings
- `recordings/`: dictation recordings, with `recordings/meetings/` holding one folder per meeting
- `models/`: Whisper `.bin` models and model files you add by hand
- `logs/`: log files

Models from Hugging Face, Parakeet Ultra among them, are kept in the shared Hugging Face cache (`~/.cache/huggingface/hub`), so other tools can reuse them. Meeting notes go to the Obsidian vault you set.

## Build from source

You need [Rust](https://rustup.rs/) and [Bun](https://bun.sh/). Platform packages are listed in [BUILD.md](BUILD.md).

```bash
git clone https://github.com/MaryanPrydatko/murmur.git
cd murmur
bun install
mkdir -p src-tauri/resources/models
curl -L -o src-tauri/resources/models/silero-vad-v6.2-F32.gguf https://huggingface.co/handy-computer/silero-vad-v6.2-gguf/resolve/main/silero-vad-v6.2-F32.gguf
bun run tauri dev      # run in development
bun run tauri build    # build the installers
```

## Help

- [Troubleshooting and platform notes](docs/troubleshooting.md)
- [Report a bug](https://github.com/MaryanPrydatko/murmur/issues)

## Credits

- [Handy](https://github.com/cjpais/Handy) by CJ Pais, which Murmur is built on
- [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) for local speech recognition
- **Whisper** by OpenAI for the speech recognition model
- **ggml** for the cross-platform tensor library
- **Silero** for the lightweight VAD
- **Tauri** for the Rust-based app framework

## Licence

MIT, see [LICENSE](LICENSE). Murmur's name, logo and icon are its own. Handy's name, logo and brand assets belong to Handy.
