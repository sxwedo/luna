# luna

**The remaining light of your LLM quotas.**

A fast Rust CLI for watching every account, every window — Gemini 5H, weekly, GLM — without a spreadsheet, a browser tab, or a table from 2014.

Login once. `luna status` after that. Consuming accounts grow a travelling highlight: it sweeps the title, orbits the card, and returns.

```text
  LUNA · remaining light                                          ● 4 active   16:02:44
  ────────────────────────────────────────────────────────────────────────────────────

  ╭─ you@gmail.com · Google Antigravity · Google AI Pro ──────────────────────────╮
  │                                                                              │
  │   ⚡ Gemini 5H            ████░░░░░░░░░░░░░░░░   15.8%     resets in 3h 43m   │
  │     Gemini Weekly        ██████████░░░░░░░░░░   52.2%     resets in 5d 18h   │
  │   ⚡ Claude/GPT 5H        ████████████████████  100.0%     resets in 4h 59m   │
  │     Claude/GPT Weekly    ████████████████████  100.0%     resets in 6d 23h   │
  │                                                                              │
  ╰──────────────────────────────────────────────────────────────────────────────╯
```

## Install

Requires a recent Rust toolchain ([rustup](https://rustup.rs)).

```bash
cargo install --git https://github.com/sxwedo/luna --locked
```

From a clone:

```bash
git clone https://github.com/sxwedo/luna.git
cd luna
cargo install --path . --locked
```

The release binary is a single file, no runtime.

## Config

OAuth clients are **not** in the repo or the binary. Put them in `~/.config/luna/config.toml`:

```toml
[antigravity]
client_id = "....apps.googleusercontent.com"
client_secret = "..."
```

The file is created empty on first use if missing. Permissions are `0600`.

## Usage

```bash
luna login                         # pick a provider, then sign in
luna sniff                         # import Antigravity from macOS Keychain
luna status                        # cards (default)
luna status --watch                # live TUI, refresh every 60s
luna status --format table
luna status --format json          # scripts, Raycast, CI
luna status -p antigravity
luna list
luna logout                        # interactive; --account for scripts
```

`login` is interactive. It does not default to Google. Current login surface: **Google Antigravity** (OAuth PKCE) and **智谱 GLM** (API key).

## What it reads

| Provider | Auth | Windows |
| --- | --- | --- |
| Google Antigravity | OAuth, Keychain sniff | Gemini 5H / Weekly, Claude/GPT 5H / Weekly |
| 智谱 GLM | API key (bigmodel.cn / z.ai) | session + search quota |

Antigravity talks to `retrieveUserQuotaSummary` (daily first, then sandbox, then prod). Live 5H beats stub 100% windows. Replica jitter inside the same reset cycle is clamped so a bar does not flicker 1.4% ↔ 1.9%.

OAuth lives in `~/.config/luna/config.toml`. Accounts live in the OS config dir (`dev.sxwedo.luna`). A vault from the earlier `quotactl` path is copied on first run.

## Watch

`luna status --watch` opens an alternate-screen TUI.

- Dual column above ~140 cells, otherwise one
- Names are not truncated (`Claude/GPT Weekly` stays whole)
- Bars fill the leftover width
- `r` refreshes now, `q` quits
- An account that is actually draining gets **CONSUMING**: one beam of light, title then border, not a rainbow frame

## Why luna

Quota is a waning moon. The number you care about is never what you spent — it is what is still lit. luna names that remainder, and draws it.

## License

[MIT](LICENSE) © sxwedo
