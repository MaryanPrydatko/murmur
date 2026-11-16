# Status and handover

Murmur is developed on two machines, a Linux PC and a Mac, in separate agent
sessions that do not share memory. This file is the shared memory. It is the
first thing to read and the last thing to update.

## The rule

- **Start of a session:** `git pull`, then read this file.
- **End of a session:** update "Now", "Next" and "Log" below, commit, push.
  Work that is not finished goes on a pushed branch, and "Now" names that
  branch and says what is left.
- Write facts, not plans that might change: what was done, what was verified
  and how, what was not verified.
- This repo is public. No personal data, no meeting content, no secrets, no
  home folder listings.

## Prompts

For the owner to paste. They make a session follow the rule above.

Start on the Mac (in `~/Developer/murmur`):

```
Murmur, my dictation and meeting app. Run git pull, then read docs/STATUS.md and AGENTS.md. That file is the whole context: state, open bugs, rules for tests and releases. Tell me in a few lines where things stand, then wait for what I want to do.
```

Before leaving either machine:

```
I'm switching machines. Update docs/STATUS.md (Now, Next, Log): what changed, what you verified and how, what is not verified, which branch holds unfinished work. Commit and push. Tell me the branch and the commit.
```

Back on the PC (in `~/Desktop/projects/murmur`):

```
Murmur. I worked on the Mac in between. Run git fetch and git pull, read docs/STATUS.md and the git log since v0.2.0, and tell me what changed. Then bring my Murmur project note in Obsidian up to date with it.
```

## Now (16.11.2026, written on the PC)

- Released: **0.2.0** (tag `v0.2.0`, same commit as `main` before this file).
  The release has a `.deb` (Ubuntu 26.04+) and a `.dmg` (Apple Silicon). Do
  not use 0.1.0: "Transcribe again" on an imported meeting without audio
  wipes its transcript there.
- Installed: 0.2.0 on the PC (package `murmur`). On the Mac
  (`/Applications/Murmur.app`) a build of `fix/meeting-echo`, see below.
- Open branch `fix/meeting-echo` (Mac, not pushed, not merged): a meeting
  recorded without headphones had the other side's words twice, once on the
  system track and again on the mic, which hears the speakers. The merge now
  drops the mic's echo of the system track (`find_echo` in
  `managers/meetings/transcript.rs`). Verified: 24 transcript tests, the
  full Rust suite, and a local replay on the kept words of the real meeting
  (not in the repo): about 1,170 of 1,480 mic words dropped as echo, every
  sentence of the owner's own kept, about 17 one-word leftovers. Not
  verified: a two-track meeting with headphones (none recorded; a meeting
  only counts as having echo when 25% of the system words match). A build of
  this branch is installed on the Mac in place of 0.2.0 and redid that meeting
  on start: 1,169 of 1,483 mic words dropped, speaker names kept, new summary.
  Its windows on screen were not checked by the agent.

### Never seen working by a person

All tests so far ran in private headless sessions with synthetic voices.

- A real voice on the microphone track of a meeting.
- Dictation pasting into real windows on the PC desktop.
- The Mac app under its own permissions (microphone, Accessibility, system
  audio), and any window of it on the Mac screen, the notch island included.

The owner tries these first. What breaks there decides 0.2.1.

### Known and not fixed

- Meeting player: after a jump back it may play the wrong place.
  **Unconfirmed**: the test meeting repeated its clips, so the measurement may
  be wrong, and the check that should have settled it did not finish.
- SIGUSR1 ends the app on Linux.
- A meeting longer than about two hours does not fit the local summary model
  in one request. Summarising in parts is not built.
- No "Summarising..." line while the automatic summary runs.
- "Summarise again" replaces an imported summary and keeps no copy.
- Overlapping speech becomes one-word turns.
- Without headphones, the owner's own one to three words said over the other
  side next to echo can be dropped when they are fillers or spelled like a
  system word nobody took; a few misheard echo words stay as one-word turns.
- The tray icon does not change during a meeting.
- `murmur --toggle-meeting` does nothing when the app is not running.
- A cancelled model download leaves a partial file.
- Spoken formatting (lists, "new line") was tuned on synthetic voices and has
  false positives: "Old line, new line, it does not matter", two "bullet
  point" mentions in one sentence, short German sentences. Setting:
  `spoken_formatting_enabled`.
- The `.deb` is built on Ubuntu 26.04 and needs glibc 2.43. A package for
  older systems needs a build on an older system. GitHub Actions are switched
  off in this repo.
- `AGENTS.md` (below its first section) and `.github/workflows` still
  describe Handy.

## Next

Order is a suggestion, the owner decides.

1. Whatever the first real use turns up.
2. Confirm or dismiss the meeting player jump.
3. Summaries of long meetings in parts.
4. The small ones: "Summarising..." line, a copy before "Summarise again",
   SIGUSR1, tray icon during a meeting.
5. A `.deb` for older Ubuntu through CI.
6. Clean `AGENTS.md` and `.github/workflows`.

Not planned unless asked: voice commands on selected text, per-app modes,
detecting a call automatically, a public launch, paid Apple signing.

## How the work is done

- New work goes on a branch. `main` only gets what is tested, and the owner
  approves every merge into `main`.
- A release is a tag, plus the `.deb` built on the PC and the `.dmg` built on
  the Mac, both uploaded to the GitHub release by hand and downloaded back to
  compare checksums. The `.deb` command is in `BUILD.md` under "Release .deb".
  One machine cannot build the other's package.
- Updates inside the app are off (`HAS_OWN_RELEASE_FEED = false`,
  `createUpdaterArtifacts: false`). There is no update feed.
- The Mac build is ad-hoc signed. macOS asks for the permissions again after
  every update. An agent must not open the Mac app to test it: the prompts
  need the owner.
- Tests that open windows, click, type or play sound never run on the live
  desktop. On Linux they run in a private headless session with its own audio
  server and no access to `/dev/uinput`. Earlier tests on the live desktop
  sent stray clicks into other apps.
- Never write into the real app data folder or a real notes vault in a test.
  Use a separate profile. The import sources (other dictation apps) are read
  only.
- Deleting branches on GitHub is left to the owner.

## What each machine has

Only what a session needs to know. Paths are the defaults.

|                    | PC (Ubuntu 26.04, GNOME Wayland)                                             | Mac (Apple Silicon)                                                         |
| ------------------ | ---------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| Checkout           | `~/Desktop/projects/murmur`                                                  | `~/Developer/murmur`                                                        |
| App data           | `~/.local/share/com.maryan.murmur`                                           | `~/Library/Application Support/com.maryan.murmur`                           |
| Speech             | Parakeet Ultra through Vulkan                                                | Parakeet Ultra through Metal                                                |
| System audio       | PipeWire sink monitor                                                        | Core Audio process tap (macOS 14.2+), vendored crates in `src-tauri/vendor` |
| Overlay            | bottom pill, needs the X11 backend under GNOME                               | notch island                                                                |
| Shortcut and paste | `keyboard_implementation: handy_keys`, `ydotool` and `wl-clipboard`          | system default                                                              |
| Summary model      | Qwen3 14B through `llama-server`, started on demand                          | Qwen3 4B through `llama-server`, launchd agent                              |
| Summary endpoint   | `http://127.0.0.1:8177/v1`, model `murmur-summary`, as the "custom" provider | the same                                                                    |
| Old meetings       | none                                                                         | imported from three other apps                                              |

Both summary servers cap an answer at 1500 tokens. A capped summary shows an
error, not half a summary. The Mac has 16 GB of memory and is tight with the
model loaded.

## Log

Newest first. One line per session: date, machine, what changed.

- 16.11.2026, Mac: first real meeting showed the mic's echo of the call;
  branch `fix/meeting-echo` drops it. Checkout moved to `~/Developer/murmur`.

- 16.11.2026, PC: this file added, with the prompts for switching machines.
- 16.11.2026, PC and Mac: 0.2.0 released. Delete a meeting, speaker model
  downloads with the first meeting, spoken lists and "new line" in dictation,
  meeting summaries through a local or cloud provider (off by default), the
  transcript-wipe fix, 24 review fixes. Packaged test on Linux passed. On the
  Mac: compiled, 467 tests, tested from a terminal in a separate profile.
- 15.11.2026, PC and Mac: 0.1.0 released from a fresh history on top of
  Handy v0.10.1. Meeting mode, import of old meetings, the Mac build, the
  logo.
