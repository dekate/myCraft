# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

myCraft: a simpler clone of Laragon (Windows full-stack dev box), written in Rust + egui (eframe 0.30). The Rust crate lives in `app/`; `docs/` holds the README screenshot.

## Commands (run from `app/`)

- Build: `cargo build`
- Run: `cargo run`
- Release exe: `cargo build --release` → `target/release/mycraft.exe` (GUI subsystem, no console; `build.rs` embeds `assets/logo.png` as the exe icon via `winresource`).
- Screenshot: `cargo run -- --screenshot <out.png>` — starts all services first, then renders and saves a screenshot.
- Release: bump `version` in `app/Cargo.toml` and push to `main`; `.github/workflows/release.yml` tests and builds Windows (x64, arm64 `.exe`), macOS (universal `.app` zip) and Linux (x86_64, aarch64 `.tar.gz`), then drafts GitHub release `v<version>` with `SHA256SUMS.txt` and notes from `.github/scripts/release-notes.sh` (commits since the last release tag, new contributors). Publish the draft by hand. Re-runs replace a draft's files; a published version is skipped.
- Tests: `cargo test` (unit tests for AI routing, classifier, key encryption in `ai.rs`). Use `cargo clippy` / `cargo fmt` for lint/format.

## Architecture

Modules in `app/src/`:

- `main.rs` — parses `--screenshot`, builds `MyCraftApp`, spawns the two background servers, opens a 540×450 window with `assets/logo.png` as icon.
- `state.rs` — `MyCraftState`: all app data (services, ports, `www_root`, virtual hosts, quick-app templates, log entries, config modal, active tab) plus actions like `start_all()`. Servers are controlled via shared `Arc<AtomicBool>` running flags (`server_running_flag`, `mailpit_running_flag`) owned here.
- `single_instance.rs` (Windows) — single-instance enforcement via named Mutex + Event with NULL DACL (works across admin/user integrity levels). If launched when already running (e.g. hidden in tray), signals the running instance to unhide, restore (`ShowWindow(SW_RESTORE)`), and focus (`SetForegroundWindow`), then exits immediately.
- `server.rs` — hand-rolled `std::net::TcpListener` servers (no web framework), each on its own thread, gated by the running flags:
  - HTTP server on `web_port` serving `www_root` files, a hub dashboard at `/`, and an Adminer-style DB page (with fallback).
  - Mailpit clone: web UI on `mailpit_port` + SMTP sink on port 1025, capturing `CapturedEmail`s.
- `app.rs` — `MyCraftApp` implements `eframe::App`; all UI (tabs, service rows, vhosts, quick app, logs, modals) renders from and mutates `MyCraftState`.
- `ai.rs` — AI chat engine (`AiChat`, held in `MyCraftState.ai`): user-added providers/keys, ordered model routes, routing modes (failover, round robin, auto small/big via `classify`, single), HTTP calls via `ureq` on worker threads polled each frame. Two wire formats: Anthropic Messages and OpenAI-compatible chat completions. Config at `%APPDATA%\myCraft\ai.json`, API keys DPAPI-encrypted.
- `ai_tab.rs` — UI for the AI tab and the Providers & Routing window.
- `tray.rs` (Windows) — system-tray icon and right-click menu (`tray-icon`). Clicks are queued as `TrayCmd`s and run in `MyCraftApp::update`. "Hidden to tray" means minimized + `WS_EX_TOOLWINDOW` (no taskbar button), not `SW_HIDE`: a hidden window never gets `update` calls on Windows, so tray commands would stall. Quitting goes through `MyCraftState::shutdown()`, which stops services and tunnels first.
- `tunnel.rs` — public tunnels for projects and the shared per-project request handler.
- `settings.rs` — ports and preferences in `<root>\usr\mycraft.json` (including customizable virtual host domain, default `{name}.go`); the server threads read `settings::current()` every loop, so saved ports or domain take effect without a restart. Start-with-Windows is the per-user Run key.
- `ssl.rs` — local CA in `etc\ssl\mycraft-ca.{crt,key}` (rcgen); one server cert for localhost + every project domain, re-issued when the project list changes; served by rustls on the HTTPS port. "Trust" adds the CA to the current user's Root store via certutil.
- `redis.rs` — Redis runner like `mysql.rs` (`conf\redis.conf`, `data\redis`, `logs\redis.log`), plus `redis-cli` calls.
- `quickapp.rs` — Quick App jobs on a worker thread (WordPress zip SHA-1 checked + wp-config, Laravel via Composer + .env + migrate, Next.js via create-next-app, static); results go back over `MyCraftState::bg_tx` and are drained each frame.

UI starts/stops services by flipping state flags; the server threads are spawned once at startup and poll those flags rather than being respawned.

## Notes

- Clean-room rule: don't read or analyze Laragon's binaries; build features from public docs and behavior only.
- The myCraft root is the folder holding `mycraft.exe` (`main.rs::mycraft_root()`): projects in `<root>\www`, PHP builds in `<root>\bin\php`; `ensure_layout()` creates them on first run. Exes under a cargo `target\` dir use an existing `G:\LARAGON` / `C:\laragon` instead.
- Services read real installs from `bin\<section>` (`Service::bin_section`, `MyCraftState::detect_versions`); none hardcoded. Missing ones offer installs from `<root>\usr\packages.conf` (`packages.rs`; default written on first run): `name=auto` resolves php-/node-/mariadb-/heidisql/composer from official feeds with SHA-256 (a fresh PHP gets a php.ini from `php_update::ensure_ini`), else `name=https-url [sha256=…]`.
- MySQL/MariaDB really runs (`mysql.rs`): the row's selected `bin\mysql\<install>`, data in `data\<family>-<major.minor>` (initialized on first start), settings `conf\my.ini`, log `logs\mysql.log`. Refuses to start if the port is taken (e.g. Laragon's own server). Live test: `cargo test -- --ignored live_server` with `MYCRAFT_TEST_MYSQL`/`MYCRAFT_TEST_FOLDER`.
- PHP per project (`php_update::for_project`): `<project>\.php-version` pin, else composer.json `require.php`, else newest installed; never run a project on a PHP it didn't ask for. `php_update.rs` also updates each installed line to its newest patch from windows.php.net (SHA-256 checked).
- egui's bundled fonts lack many symbols (e.g. `●`, `🟢`); check a glyph exists in `epaint_default_fonts` before using it in UI text (`⚫` works). Fonts are embedded with `include_bytes!` from `app/assets/fonts/` (`main.rs::fonts()`): Inter is the UI font, Inter SemiBold is the `"bold"` family, then NotoEmoji, emoji-icon-font, Noto Sans Symbols 2 and Hack as fallbacks. Before using a new symbol, check it against these files. Never load fonts or other UI assets from the network; the app must work offline.
- AI replies render with `egui_commonmark` 0.19 (the release for egui 0.30), with default features off so it can't load images or fetch anything. `app/vendor/egui_commonmark_backend` is a patched copy (via `[patch.crates-io]`) that draws bold with the `"bold"` family; see its `MYCRAFT_PATCH.md`. Upgrading egui means upgrading both.
- Background console tools must be spawned through `crate::hide_console` so no console window flashes.
- No fake success: every button and log line reports what really happened. If something isn't implemented, don't show it.
- The web server serves port 80 and HTTPS only when nothing else answers there at start (`server::SERVE_80` / `SERVE_HTTPS`), so it never hijacks Laragon's Apache. Requests go to `logs\access.log`, php-cgi stderr to `logs\php_error.log`.
- Live tests (`cargo test -- --ignored <name> --nocapture`): `live_web` (HTTP + HTTPS on 18080/18443), `live_redis` (`MYCRAFT_TEST_REDIS`), `live_quickapp` (static + WordPress download), `live_feeds`, `live_server`. Test exes live in `target\debug\deps`, so `mycraft_root()` is that folder in tests.
