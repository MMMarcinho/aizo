# aizo 爱憎

[中文文档](README.zh.md)

**aizo** (爱憎, *ài zēng*, "love and hate") is a lightweight, high-performance preference memory system for AI agents, built entirely in Rust.

It mimics human cognitive memory: rather than storing full conversation transcripts, it continuously **extracts, quantifies, decays, and recalls** a user's stable preferences, aversions, habits, communication styles, and hard limits from interaction history. The result is a compact, numerically-weighted personality profile that any agent can query in milliseconds.

---

## How it fits together

aizo is designed for two complementary usage loops:

```
╔══════════════════════════════════════════════════════════════════════╗
║  1. In-session  (reactive — detects specific emotions in real time)  ║
╚══════════════════════════════════════════════════════════════════════╝

   user ──► Claude Code ─────── aizo add ──────────────────┐
                                                           ▼
                             CLAUDE.md ◄── contributes ── local SQLite
                                                      (user preference)


╔══════════════════════════════════════════════════════════════════════╗
║  2. Background  (cron task — batch-analyzes accumulated sessions)    ║
╚══════════════════════════════════════════════════════════════════════╝

   user ──► openclaw ──► sessions ─── aizo analyze ─────────┐
                                                            ▼
   USER.md, SOUL.md, IDENTITY.md … ◄── contributes ── local SQLite
                                                      (user preference)
```

**Loop 1 — In-session:** the agent detects a strong preference signal mid-conversation
(praise, complaint, explicit rule) and calls `aizo add` immediately. The updated SQLite
profile is then injected into `CLAUDE.md` (or equivalent context file) so the next
session starts with the latest understanding of the user.

**Loop 2 — Background:** other agents (openclaw, etc.) accumulate session transcripts
over time. A scheduled cron job runs `aizo analyze` to extract implicit preferences the
reactive loop may have missed. The enriched profile is then written into richer identity
files — `USER.md`, `SOUL.md`, `IDENTITY.md` — that build a persistent, evolving
picture of the user across all agents and tools.

The two loops reinforce each other: reactive writes give immediate recall accuracy;
batch analysis fills in the gaps and stabilises scores over time.

---

## Core design

```
session transcript  (text, JSON, JSONL)
       │
       ▼
  flash LLM (any OpenAI-compatible or Anthropic)
       │  semantic extraction
       ▼
  structured entries  { item, base_score 0–10 }
       │  smooth merge  (old×0.4 + new×0.6)
       ▼
  SQLite (~/.aizo/preferences.db)
       │
       ▼
  effective_weight = s · d(t)^α   (score-modulated decay)
       │  keyword / type / scenario recall
       ▼
  agent reads profile → personalizes response
```

### Scoring formula

All scoring logic lives in `src/scoring/mod.rs`. Every preference entry carries three computed fields, derived at read time from its `base_score` and `last_seen` timestamp.

**Step 1 — Decay coefficient** $d(t)$

$$d(t) = \phi + (1 - \phi) \cdot e^{-\lambda t}, \quad \lambda = \frac{\ln 2}{t_{1/2}}$$

where $t$ is days since `last_seen`, $t_{1/2}$ is the configured half-life, and $\phi$ is the floor.

**Step 2 — Score-dependent exponent** $\alpha$

$$\alpha = \frac{10 - s}{10}$$

Higher score → smaller $\alpha$ → decay has less effect. A score-10 preference ($\alpha = 0$) is fully decay-resistant; a score-0 entry ($\alpha = 1$) decays at full speed.

**Step 3 — Effective weight** $w$

$$w = s \cdot d(t)^{\alpha}$$

Expanding into a single expression:

$$\boxed{w = s \cdot \left[\phi + (1-\phi) \cdot e^{-\lambda t}\right]^{\frac{10-s}{10}}}$$

**Boundary behaviour**

| Score $s$ | $\alpha$ | Decay effect | Interpretation |
|---|---|---|---|
| 10 | 0.0 | None — $d^0 = 1$ | Core value, never fades |
| 7  | 0.3 | Slight | Strong preference, slow fade |
| 5  | 0.5 | Moderate | Neutral habit, fades at half speed |
| 1  | 0.9 | Near-full | Weak aversion, fades quickly |
| 0  | 1.0 | Full | $w = 0$ always — absolute zero |

Entries are **never hard-deleted by decay** — they sink toward the floor and persist as weak long-term memory. Use `aizo recall --type taboo` to surface hard limits regardless of effective weight.

### Score smoothing

When the same entry is seen again across sessions:
```
new_base_score = old_base_score × 0.4 + incoming_score × 0.6
```
`last_seen` is always refreshed, which resets the decay clock.

---

## Installation

```bash
# Cargo (recommended)
cargo install aizo

# npm / npx
npm install -g aizo
npx aizo top 10

# From source (Rust ≥ 1.70)
git clone https://github.com/mmmarcinho/aizo
cd aizo && cargo build --release
cp target/release/aizo /usr/local/bin/aizo
```

### First-time setup

Run the interactive wizard — it writes `~/.aizo/.env` and tests the connection:

```bash
aizo init
```

Or set env vars manually:

```bash
# Anthropic
export ANTHROPIC_API_KEY=sk-ant-...

# Any OpenAI-compatible / local model (Ollama, OpenRouter, DeepSeek, vLLM…)
export AIZO_MODEL=qwen2.5:7b
export AIZO_API_URL=http://localhost:11434/v1/chat/completions
```

### Configuration env vars

| Variable | Default | Description |
|---|---|---|
| `AIZO_DB_PATH` | `~/.aizo/preferences.db` | SQLite database path |
| `ANTHROPIC_API_KEY` | — | Anthropic API key (auto-detected) |
| `AIZO_API_KEY` | — | API key for any provider |
| `AIZO_API_URL` | provider default | LLM endpoint URL |
| `AIZO_MODEL` | `claude-haiku-4-5` | Model name |
| `AIZO_API_FORMAT` | auto | `anthropic` to force Anthropic wire format |
| `AIZO_MAX_TOKENS` | `8192` | Max output tokens for LLM response |
| `AIZO_AUTO_KEYWORDS` | `false` | `true` to auto-generate keywords via LLM |

All vars can be set in `~/.aizo/.env` (user-wide) or `./.env` (per-project). Shell env always wins.

---

## CLI reference

```
aizo [--db <path>] <COMMAND>
```

| Command | Description |
|---|---|
| `init` | Interactive setup wizard — writes `~/.aizo/.env`, tests connection |
| `analyze [file]` | Analyze session file or JSON/JSONL export with LLM |
| `extract [file]` | Print extraction prompt to stdout (pipe to any LLM) |
| `import` | Read `{"entries":[…]}` JSON from stdin and upsert entries |
| `recall [query]` | Keyword + score-range recall — **primary agent call** |
| `top [N]` | Top-N entries by effective weight (default 10) |
| `show` | Full profile sorted by effective weight |
| `add <item> <reason>` | Manually add or update a preference |
| `tag <item> <keywords…>` | Add or replace keywords on an existing entry |
| `touch <item…>` | Reset decay clock without changing score |
| `remove <item…>` | Hard-remove an entry |
| `keywords` | List all stored keywords with entry counts |
| `clear` | Wipe entire profile and session history |
| `info` | DB path, score distribution, env config, decay settings |
| `config show/set-half-life/set-floor` | Get or set decay parameters |

**`recall` flags:**

| Flag | Description |
|---|---|
| `--type/-t <types>` | Score-range filter, comma-separated: `preference`, `style`, `habit`, `aversion`, `taboo` |
| `--limit/-l <N>` | Cap results after sorting by effective weight |
| `--scenario <name>` | Expand to preset keyword list: `coding`, `writing`, `communication` |
| `--no-touch` | Do not refresh `last_seen` for matched entries |
| `--json` | Output raw JSON instead of human-readable text |

**`top` / `show` / `recall` flags:** `--json` outputs raw JSON for agent consumption.

**`top` flags:** `--type/-t` same score-range filter as recall.

### Score guide

There is no `category` field. The `base_score` is the only dimension that matters:

| Score | Meaning | `--type` alias |
|---|---|---|
| 0–1.5 | Hard limit / must never do | `taboo` |
| 1.6–4 | Clear dislike | `aversion` |
| 4–6.5 | Neutral habit or weak pattern | `habit` |
| 6.5–10 | Style / communication preference | `style` |
| 7–10 | Clear preference | `preference` |

Use `--type` on `recall` and `top` to filter by score range. Comma-separate for multi-type:

```bash
aizo recall code --type preference,habit,style,taboo
aizo recall --type taboo               # all hard limits, no keyword needed
aizo top 5 --type preference
```

Use keywords (`--keywords` on add, or `aizo tag`) to add any taxonomy you want.

### Examples

```bash
# Analyze a session log
aizo analyze ./chat.txt
cat conversation.md | aizo analyze

# Agent recalls preferences before generating
aizo top 5
aizo recall "code style"

# Scenario-aware recall for coding tasks (expands to ~10 coding keywords)
aizo recall --scenario coding --type preference,style,habit,taboo --limit 20

# Type-only recall (no keyword — returns all entries in that score range)
aizo recall --type taboo                        # all hard limits
aizo recall code --type preference --limit 10   # top coding preferences
aizo recall code --type preference,habit --limit 20  # multiple types

# Inspect full profile
aizo show

# Manual entries — score encodes sentiment
aizo add "concise code"     "Always asks for shorter implementations"  --score 9.0
aizo add "verbose comments" "Complained about over-documented code"    --score 1.5
aizo add "emojis in output" "Explicitly said never use emojis"         --score 0.5
aizo add "uses dark mode"   "Mentioned dark theme in every UI session" --score 5.0
aizo add "terse naming"     "Consistently chose short variable names"  --score 8.0

# Add or manage keywords for richer recall
aizo tag "concise code" brevity minimal short lean
aizo tag "verbose comments" verbosity docs comments over-engineering

# Tune decay (default: half-life 30d, floor 0.1)
aizo config set-half-life 14
aizo config set-floor 0.05

# Stats
aizo info
```

---

## Entry format

```json
{
  "id": 1,
  "item": "concise code",
  "reason": "Always asks for shorter implementations with no fluff.",
  "keywords": ["brevity", "minimal", "short", "lean"],
  "base_score": 9.0,
  "source": "analysis",
  "added_at": "2026-05-07T14:00:00+00:00",
  "last_seen": "2026-05-07T15:30:00+00:00",
  "score_exponent": 0.1,
  "decay_coefficient": 0.87,
  "effective_weight": 7.83
}
```

---

## Database schema

```sql
CREATE TABLE preferences (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    item        TEXT    NOT NULL,
    reason      TEXT    NOT NULL,
    keywords    TEXT    NOT NULL DEFAULT '',    -- comma-separated synonym tags
    base_score  REAL    NOT NULL DEFAULT 5.0,   -- 0-10
    source      TEXT    NOT NULL DEFAULT 'manual',
    added_at    TEXT    NOT NULL,
    last_seen   TEXT    NOT NULL                -- resets decay clock on each reinforcement
);
-- UNIQUE on LOWER(item)

CREATE TABLE decay_config (
    id              INTEGER PRIMARY KEY CHECK(id = 1),
    half_life_days  REAL    NOT NULL DEFAULT 30.0,
    floor           REAL    NOT NULL DEFAULT 0.1
);

CREATE TABLE sessions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    analyzed_at  TEXT    NOT NULL,
    extracted    INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT    NOT NULL DEFAULT ''  -- FNV-1a hash for dedup
);
```

---

## Agent integration

Any agent can call aizo as a subprocess — no embedding, no vector index, no runtime:

```python
import subprocess, json

def top_preferences(n: int = 10) -> list[dict]:
    return json.loads(subprocess.check_output(["aizo", "top", str(n), "--json"]))

def recall(query: str, types: str = "preference,style,habit,taboo") -> list[dict]:
    return json.loads(subprocess.check_output(
        ["aizo", "recall", query, "--type", types, "--json"]
    ))

def recall_scenario(scenario: str) -> list[dict]:
    return json.loads(subprocess.check_output(
        ["aizo", "recall", "--scenario", scenario,
         "--type", "preference,style,habit,taboo", "--limit", "20", "--json"]
    ))

# Inject into system prompt before generating
prefs = top_preferences(20)
system = f"User preferences:\n{json.dumps(prefs, indent=2)}\n\n{base_system}"

# Before writing code, check coding preferences
coding_prefs = recall_scenario("coding")
```

Or configure `AIZO_DB_PATH` per-project to maintain separate profiles:

```bash
export AIZO_DB_PATH=./project-prefs.db
aizo show
```

---

## Standard Operating Procedure (SOP)

The SOP for how an agent should use aizo is defined as a skill file at
`skills/aizo-sop.md`. Copy it into your agent's skill/instruction directory
(e.g. `.claude/skills/` for Claude Code) and any agent in that project will
automatically follow the protocol.

The skill defines seven triggers:

| # | Trigger | aizo call | Timing |
|---|---|---|---|
| 1 | Session starts | `aizo top 20` → format as prose header | Sync, before first reply |
| 2 | User shows negative feedback | `aizo add … --score 1.5` then `aizo recall <topic>` | Sync, before corrected reply |
| 3 | User praises something | `aizo add … --score 9.0` | Async, after reply sent |
| 4 | User states an explicit rule | `aizo add … --score 0.5` or `--score 10` | Sync, immediate |
| 5 | About to generate on topic X | `aizo recall --scenario <X>` or `aizo recall <X> --type preference,style,taboo` | Sync, before generation |
| 6 | Session ends | `aizo analyze <transcript>` | Async, background |
| 7 | Daily cron job | Agent LLM scans logs → `aizo touch` confirmed items | Scheduled, background |

**Key rules encoded in the skill:**
- Taboos always win over preferences in conflicts
- `analyze` is for full sessions, not single messages — it calls an LLM
- Silence (`recall` returning nothing) means no data, not neutral preference
- Never mention aizo to the user — it runs silently

---

## Development

```bash
cargo build
cargo build --release
cargo test
```

---

## License

MIT
