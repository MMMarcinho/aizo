# aizo 爱憎

**aizo** (爱憎, *ài zēng*, "love and hate") is a lightweight, high-performance preference memory system for AI agents, built entirely in Rust.

It mimics human cognitive memory: rather than storing full conversation transcripts, it continuously **extracts, quantifies, decays, and recalls** a user's stable preferences, aversions, habits, communication styles, and hard limits from interaction history. The result is a compact, numerically-weighted personality profile that any agent can query in milliseconds.

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
  effective_weight = base_score × decay_coefficient
       │  keyword or top-N recall
       ▼
  agent reads profile → personalizes response
```

### Time-decay mechanism

Human memory fades. aizo replicates this with an **exponential decay** applied to every entry at read time:

```
decay_coefficient = floor + (1 − floor) × exp(−λ × days_inactive)
                where λ = ln(2) / half_life_days

effective_weight = base_score × decay_coefficient
```

| Days inactive | Coefficient (default: half-life=30d, floor=0.1) |
|---|---|
| 0  | 1.00 |
| 30 | 0.55 |
| 60 | 0.33 |
| 90 | 0.21 |
| ∞  | 0.10 (floor — never zero) |

Entries are **never hard-deleted by decay** — they sink to the floor and persist as weak long-term memory.

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
| `add <category> <item> <reason…>` | Manually add or update a preference |
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

# Manual entries
aizo add love "concise code"    "Always asks for shorter implementations"
aizo add hate "verbose comments" "Complained about over-documented code multiple times"
aizo add taboo "emojis in output" "Explicitly said never use emojis"
aizo add habit "uses dark mode"  "Mentioned dark theme in every UI session"
aizo add style "terse naming"    "Consistently chose short variable names"

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

def recall(query: str) -> list[dict]:
    return json.loads(subprocess.check_output(["aizo", "recall", query]))

# Inject into system prompt before generating
prefs = top_preferences(5)
system = f"User preferences:\n{json.dumps(prefs, indent=2)}\n\n{base_system}"
```

Or configure `AIZO_DB_PATH` per-project to maintain separate profiles:

```bash
export AIZO_DB_PATH=./project-prefs.db
aizo show
```

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
