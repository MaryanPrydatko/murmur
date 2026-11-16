# Murmur

Local dictation and meeting notes for Linux and macOS.

Press a shortcut, speak, and the text is typed into the app you are in. Start a meeting and Murmur records it, writes down who said what, and puts a note in your Obsidian vault. Recording, transcription and speaker labels run on your machine, and text is sent to an AI provider only if you set one up.

Murmur is a fork of [Handy](https://github.com/cjpais/Handy) by CJ Pais.

## What it does

- **Dictation.** Hold or tap a global shortcut, talk, and the transcript is pasted where your cursor is.
- **Spoken formatting.** When you dictate in English, "new line", "new paragraph", "bullet point" and "number one, ... number two, ..." become line breaks and lists. A command only counts when you say it as its own phrase, with a short pause around it, so "a new line of code" stays as it is. It is on by default and can be turned off in Settings > Advanced. With the Direct or External Script paste method the text stays on one line.
- **Meetings.** Records your microphone and the computer's audio as two tracks, then transcribes both. On a Mac the computer's audio needs macOS 14.2 or newer.
- **Speaker labels.** Your side is labelled as you. The other side is split into speakers, which you can rename. Splitting needs a speaker model (about 140 MB). If it is not on your computer yet, it is downloaded when you start a meeting. If the download fails, the other side stays one speaker and a notice after the meeting offers the download.
- **Notes to Obsidian.** Set a vault in Settings and each finished meeting is written there as a note, in the `Meetings` folder by default. A note is written once and never overwritten.
- **Meeting summaries.** Off by default. Turn on Summarise Meetings in Settings > Advanced and set an AI provider under Post Process, and each transcribed meeting gets a summary, decisions and action items. The summary is shown on the meeting's page and goes into its note. The transcript is sent to that provider. The provider can be a model on your own computer, and then nothing leaves it.
- **Local models.** Models are downloaded in the app and run on your CPU or GPU. Parakeet Ultra covers 25 European languages. Whisper and other models can be picked in Settings > Models.

## Install

Downloads are on the [Releases page](https://github.com/MaryanPrydatko/murmur/releases).

### Linux

Ubuntu 26.04 or newer: download the `.deb` and install it with APT, so its dependencies come with it:

```bash
sudo apt install ./Murmur_*_amd64.deb
```

The `.deb` needs glibc 2.43, which older systems do not have. On any other system, build Murmur from source, see [BUILD.md](BUILD.md).

The `.deb` adds Murmur to the app grid. Murmur sits in the system tray while it runs.

Pasting needs a helper tool that the packages do not install. Which one depends on your desktop, see [Linux notes](docs/troubleshooting.md#linux-notes).

### macOS

Download the `.dmg`. It is for Apple Silicon Macs. On an Intel Mac, [build from source](#build-from-source).

Murmur is not signed with a paid Apple developer certificate, so macOS blocks the first launch. To allow it once:

1. Open the `.dmg` and drag Murmur to Applications.
2. Open Murmur. macOS says it cannot be opened. Close that dialog.
3. Go to System Settings > Privacy & Security, scroll down to the message about Murmur and click **Open Anyway**.
4. Confirm with your password. After that Murmur opens like any other app.

macOS then asks for three permissions: the microphone, Accessibility (to paste the text where your cursor is) and system audio recording (to record the computer's audio in a meeting). Because Murmur is not signed with a paid certificate, macOS asks for them again after each update.

Meetings need macOS 14.2 or newer. Dictation also works on older versions.

## Use

### Dictation

The default shortcut is `Ctrl+Alt+Space` on Linux and `Option+Space` on macOS. Hold it while you talk and let go to finish, or tap it once to start and once to stop. You can change it in Settings. On macOS `Esc` cancels. On Linux there is no cancel key: cancel from the tray menu or with `murmur --cancel`.

On Wayland the desktop may not pass global shortcuts to apps. In that case bind a system shortcut to `murmur --toggle-transcription` (see [Command line](#command-line) and [Linux notes](docs/troubleshooting.md#linux-notes)).

### Meetings

Start and stop a meeting from the tray menu (**Start Meeting** / **Stop Meeting**), from the Meetings page in the app, or from a terminal:

```bash
murmur --toggle-meeting
```

When you stop, the recording is transcribed and shows up on the Meetings page, where you can play it, search it, rename speakers and open the note. You can keep dictating while a meeting is recording.

To delete a meeting, use **Delete meeting** in the menu on its page or the trash icon on its row in the list. Its recordings, transcript and summary are removed from your computer and cannot be brought back. The note in Obsidian stays. A meeting that is recording or being transcribed cannot be deleted.

### Command line

The `.deb` installs the command as `murmur`. These flags are sent to the running app:

```bash
murmur --toggle-transcription    # start or stop dictation
murmur --toggle-post-process     # start or stop dictation with post-processing
murmur --toggle-meeting          # start or stop recording a meeting
murmur --cancel                  # cancel the current operation
```

Startup flags:

```bash
murmur --start-hidden            # start without showing the window
murmur --no-tray                 # start without the tray icon
murmur --debug                   # verbose logging
murmur --help                    # all flags
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
