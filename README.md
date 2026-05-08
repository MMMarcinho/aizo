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
session transcript
       │
       ▼
  flash LLM (claude-haiku-4-5)
       │  semantic extraction
       ▼
  structured entries  { category, item, base_score 0–10 }
       │  smooth merge
       ▼
  SQLite (~/.aizo/preferences.db)
       │
       ▼
  effective_weight = s · d(t)^α   (score-modulated decay)
       │  keyword or top-N recall
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

Entries are **never hard-deleted by decay** — they sink toward the floor and persist as weak long-term memory. Use `--type taboo` to surface them explicitly regardless of effective weight.

### Scoring scale (0–10)

| Score | Meaning |
|---|---|
| 0 | Absolute taboo / hard rejection |
| 1–3 | Clear dislike / aversion |
| 4–6 | Neutral tendency / weak pattern |
| 7–9 | Clear preference |
| 10 | Strong, consistent, high-priority love |

### Score smoothing

When the same entry is seen again across sessions:
```
new_base_score = old_base_score × 0.4 + incoming_score × 0.6
```
`last_seen` is always refreshed, which resets the decay clock.

---

## Installation

### From source (Rust ≥ 1.70)

```bash
git clone https://github.com/mmmarcinho/aizo
cd aizo
cargo build --release
cp target/release/aizo /usr/local/bin/aizo
```

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # required for 'analyze'
```

---

## CLI reference

```
aizo [--db <path>] <COMMAND>
```

| Command | Description |
|---|---|
| `analyze [file]` | Analyze session file (or stdin) with flash LLM |
| `recall <query>` | Keyword recall sorted by effective weight — **primary agent call** |
| `top [N]` | Top-N entries by effective weight (default 10) |
| `show` | Full profile as JSON, sorted by effective weight |
| `add <item> <reason…> [--score N] [--type cat]` | Manually add or update a preference |
| `touch <category> <item…>` | Reset decay clock without changing score |
| `remove <category> <item…>` | Hard-remove an entry |
| `clear` | Wipe entire profile and session history |
| `info` | DB path, per-category counts, decay settings |
| `config show` | Print decay configuration |
| `config set-half-life <days>` | Set decay half-life |
| `config set-floor <0.0–1.0>` | Set minimum decay floor |

### Categories

| Category | Aliases | Default score | Meaning |
|---|---|---|---|
| `preference` | `love` | 9.0 | Consistent likes and priorities |
| `aversion` | `hate` | 1.0 | Dislikes and pet peeves |
| `habit` | — | 5.0 | Behavioral patterns, neutral |
| `style` | — | 8.0 | Communication and formatting preferences |
| `taboo` | — | 0.5 | Hard limits, must-never-do |

### Examples

```bash
# Analyze a session log
aizo analyze ./chat.txt
cat conversation.md | aizo analyze

# Agent recalls top preferences before generating
aizo top 5
aizo recall "code style"

# Inspect full profile
aizo show

# Manual entries — category inferred from score
aizo add "concise code"    "Always asks for shorter implementations"  --score 9.0
aizo add "verbose comments" "Complained about over-documented code"   --score 1.5
aizo add "emojis in output" "Explicitly said never use emojis"        --type taboo
aizo add "uses dark mode"  "Mentioned dark theme in every UI session" --score 5.0
aizo add "terse naming"    "Consistently chose short variable names"  --type style --score 8.0

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
  "category": "preference",
  "item": "concise code",
  "reason": "Always asks for shorter implementations with no fluff.",
  "base_score": 9.0,
  "source": "analysis",
  "added_at": "2026-05-07T14:00:00+00:00",
  "last_seen": "2026-05-07T15:30:00+00:00",
  "decay_coefficient": 0.87,
  "effective_weight": 7.83
}
```

---

## Database schema

```sql
CREATE TABLE preferences (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    category    TEXT    NOT NULL
        CHECK(category IN ('preference','aversion','habit','style','taboo')),
    item        TEXT    NOT NULL,
    reason      TEXT    NOT NULL,
    base_score  REAL    NOT NULL DEFAULT 5.0,   -- 0-10
    source      TEXT    NOT NULL DEFAULT 'manual',
    added_at    TEXT    NOT NULL,
    last_seen   TEXT    NOT NULL                -- resets decay clock on each reinforcement
);
-- UNIQUE on (category, LOWER(item))

CREATE TABLE decay_config (
    id              INTEGER PRIMARY KEY CHECK(id = 1),
    half_life_days  REAL    NOT NULL DEFAULT 30.0,
    floor           REAL    NOT NULL DEFAULT 0.1
);

CREATE TABLE sessions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    analyzed_at TEXT    NOT NULL,
    extracted   INTEGER NOT NULL DEFAULT 0
);
```

---

## Agent integration

Any agent can call aizo as a subprocess — no embedding, no vector index, no runtime:

```python
import subprocess, json

def top_preferences(n: int = 10) -> list[dict]:
    return json.loads(subprocess.check_output(["aizo", "top", str(n)]))

def recall(query: str, kind: str | None = None) -> list[dict]:
    cmd = ["aizo", "recall", query]
    if kind:
        cmd += ["--type", kind]
    return json.loads(subprocess.check_output(cmd))

# Inject into system prompt before generating
prefs = top_preferences(10)
system = f"User preferences:\n{json.dumps(prefs, indent=2)}\n\n{base_system}"
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

The skill defines six triggers:

| # | Trigger | aizo call | Timing |
|---|---|---|---|
| 1 | Session starts | `aizo top 20` → format as prose header | Sync, before first reply |
| 2 | User shows negative feedback | `aizo add aversion …` then `aizo recall <topic>` | Sync, before corrected reply |
| 3 | User praises something | `aizo add preference …` | Async, after reply sent |
| 4 | User states an explicit rule | `aizo add taboo/preference …` | Sync, immediate |
| 5 | About to generate on topic X | `aizo recall <X>` | Sync, before generation |
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
