# Podcast Studio

A local audio and video editor with non-destructive, transcript-based editing. The web interface runs in your browser, while a local FastAPI backend handles media, projects, and processing.

Original media files are never overwritten. Edits are saved as kept intervals in each project's edit decision list (EDL).

## Features

- Import audio and video files that FFmpeg can read.
- Generate word-timestamped transcripts locally with faster-whisper, or use Groq or Deepgram.
- Select transcript words to remove corresponding audio and video.
- Make cuts at the playhead, then undo or redo edits.
- Correct transcript text without changing the original recording.
- Add and edit topic markers in a transcript.
- Adjust audio enhancement settings and render a preview.
- Export edited media as MP4, WebM, WAV, MP3, M4A, or AAC.
- Save and reopen projects from the local project library.

## Requirements

- Git
- Python 3.12 or newer
- Node.js 20 or newer, including npm
- FFmpeg and FFprobe available on your `PATH`

Check that the required tools are available:

```bash
git --version
python3 --version
node --version
npm --version
ffmpeg -version
ffprobe -version
```

The setup and development scripts use Bash. On Windows, use WSL or another Bash environment with the required tools installed.

## Clone the repository

```bash
git clone https://github.com/NonakaVal/open-podcast.git
cd open-podcast
```

If you have already cloned the repository, change into its directory instead.

## Install dependencies

From the repository root, run:

```bash
scripts/setup.sh
```

The setup script creates a Python virtual environment at `app/backend/.venv`, installs the backend dependencies and optional test and transcription dependencies, and installs the frontend npm packages.
It also installs the `podcast-studio` command in `~/.local/bin`.

If your system's default Python is not 3.12, select a Python 3.12 executable explicitly:

```bash
PYTHON=python3.12 scripts/setup.sh
```

Make sure the selected executable is installed and available on your `PATH`.

## Start the application

Run the global command from any directory:

```bash
podcast-studio
```

The script starts both the backend and frontend. Keep the terminal open while using the app; press `Ctrl+C` to stop the development servers.

If dependencies are already installed but the command is not, run `scripts/install-global.sh` from the repository root. If your shell cannot find `podcast-studio`, ensure `~/.local/bin` is on your `PATH`.

Open the app at <http://127.0.0.1:5173>.

The local API is available at <http://127.0.0.1:8787>. Its interactive OpenAPI documentation is at <http://127.0.0.1:8787/docs>.

## Use the editor

1. Open the app and import an audio or video file.
2. Wait for the app to analyze the media and create a project.
3. Generate a transcript, if needed:
   - **Local · faster-whisper** processes the recording on your computer. Choose a model (`tiny`, `base`, `small`, or `medium`); `small` is the default recommendation.
   - **Groq · Whisper Large V3 Turbo** or **Deepgram · Nova-3** sends audio to the selected provider. Enter the provider's API key when prompted.
4. Click a transcript word to seek to it and select it. Use `Shift`+click to select a range of words. Double-click a word to correct its text; this does not change the audio.
5. Remove selected words to exclude their corresponding audio and video from the edit. You can also make a cut at the playhead. Use undo and redo to review edit changes.
6. Add topic markers to organize the transcript. You can rename or remove markers.
7. Adjust audio enhancement settings and render a preview if desired.
8. Select an export format and export the edited media.

The editor preserves the original media file. Removing words or making cuts changes the project edit, not the source recording.

## Transcription and privacy

Local transcription requires faster-whisper. The first run may need to download the selected speech model. Once available, the recording is processed locally; CUDA is used when the required libraries and hardware are available, otherwise the app falls back to CPU int8.

If the current Python version or platform does not have compatible packages, create the environment with Python 3.12:

```bash
PYTHON=python3.12 scripts/setup.sh
```

Groq and Deepgram are optional alternatives. When either provider is selected, audio is sent to that provider for transcription. For longer recordings or video, the backend extracts and sends audio in segments, then combines the returned word timestamps. Review each provider's terms, limits, and privacy policy before use. The API key is used for the request and is not saved by the app.

## Local data

Runtime data is stored under `app/backend/data/`:

- `media/` — imported media files
- `projects/` — project data and exported files
- `previews/` — generated previews
- `media.json` — the local media index

Keep this directory if you want to retain your projects and imported media. Removing a project through the app does not delete its original media file.

## Development and tests

Run the backend tests:

```bash
cd app/backend
.venv/bin/python -m pytest -q
```

Run the frontend type check and production build:

```bash
cd app/frontend
npm run typecheck
npm run build
```

The frontend development server can also be started separately with `npm run dev`. The backend can be started from `app/backend` with:

```bash
.venv/bin/uvicorn main:app --host 127.0.0.1 --port 8787
```

## Experimental Rust workspace

The `crates/` directory contains an experimental Rust workspace. It is not the main web application described above. To run its tests and lint checks, install a Rust toolchain compatible with the workspace's declared Rust version, then run these commands from the repository root:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Project structure

- `app/backend/` — FastAPI API, media processing, transcription, editing, and tests
- `app/frontend/` — React and TypeScript web application
- `app/backend/data/` — local runtime media, projects, and previews
- `crates/` — experimental Rust workspace
- `scripts/setup.sh` — installs backend and frontend dependencies
- `scripts/install-global.sh` — installs the `podcast-studio` command in `~/.local/bin`
- `scripts/dev.sh` — starts the local backend and frontend servers
