# aizo 爱憎

![aizo — preference memory for AI agents](assets/aizo-hero.png)

[中文文档](README.zh.md)

**aizo** (爱憎, *ài zēng*, "love and hate") is a lightweight, high-performance preference memory system for AI agents, built entirely in Rust.

It mimics human cognitive memory: rather than storing full conversation transcripts, it continuously **extracts, quantifies, decays, and recalls** a user's stable preferences, aversions, habits, communication styles, and hard limits from interaction history. The result is a compact, numerically-weighted personality profile that any agent can query in milliseconds.

---

## How it fits together

aizo is designed around two complementary patterns:

```
╔══════════════════════════════════════════════════════════════════════╗
║  1. In-session  (reactive — detects specific emotions in real time)  ║
╚══════════════════════════════════════════════════════════════════════╝

   user ──► agent ─────── aizo add ──────────────────┐
                                                     ▼
                       CLAUDE.md ◄── contributes ── local SQLite
                                                (user preference)


╔══════════════════════════════════════════════════════════════════════╗
║  2. Just-in-time recall  (on-demand — pulls relevant prefs per task) ║
╚══════════════════════════════════════════════════════════════════════╝

   agent gets task ──► aizo recall --scenario coding ──► session context
                                                              │
                                                              ▼
                                                  generate with preferences
```

**Loop 1 — In-session:** the agent detects a strong preference signal mid-conversation
(praise, complaint, explicit rule) and calls `aizo add` immediately. Key preferences
that should persist across all sessions are written into `CLAUDE.md` or `MEMORY.md`.

**Loop 2 — Just-in-time recall:** not all preferences belong in persistent context files.
The agent classifies each incoming task into a scenario, calls `aizo recall --scenario <X>`,
and injects only the relevant preferences into the current session context. This keeps
the agent's base context lean while giving it access to the full preference profile —
just like human working memory.

For batch analysis of historical sessions to discover new preferences, see SOP 6 in
`skills/aizo-sop.md` — this is a skill-level concern, not a built-in aizo command.

---

## Core design

```
agent observation  (praise, complaint, rule, habit)
       │
       ▼
  aizo add  { item, base_score 0–10, keywords, scenarios }
       │  smooth merge  (old×0.4 + new×0.6)
       ▼
  SQLite (~/.aizo/preferences.db)
       │
       ▼
  effective_weight = s · d(t)^α   (score-modulated decay)
       │  keyword / score-band / scenario recall
       ▼
  agent reads profile → personalizes response
```

### Scoring formula

All scoring logic lives in `src/scoring/mod.rs`. Every preference entry carries three computed fields, derived at read time from its `base_score` and `last_seen` timestamp.

**Step 1 — Decay coefficient d(t)**

$$d(t) = \phi + (1 - \phi) \cdot e^{-\lambda t}, \quad \lambda = \frac{\ln 2}{t_{1/2}}$$

where t is days since `last_seen`, t½ (half-life) is the configured half-life, and φ (floor) is the minimum decay.

**Step 2 — Score-dependent exponent α**

$$\alpha = \frac{10 - s}{10}$$

Higher score → smaller α → decay has less effect. A score-10 preference (α = 0) is fully decay-resistant; a score-0 entry (α = 1) decays at full speed.

**Step 3 — Effective weight w**

$$w = s \cdot d(t)^{\alpha}$$

Expanding into a single expression:

$$\boxed{w = s \cdot \left[\phi + (1-\phi) \cdot e^{-\lambda t}\right]^{\frac{10-s}{10}}}$$

**Boundary behaviour**

| Score s | α | Decay effect | Interpretation |
|---|---|---|---|
| 10 | 0.0 | None (d⁰ = 1) | Core value, never fades |
| 7  | 0.3 | Slight | Strong preference, slow fade |
| 5  | 0.5 | Moderate | Neutral habit, fades at half speed |
| 1  | 0.9 | Near-full | Weak aversion, fades quickly |
| 0  | 1.0 | Full (w = 0) | Absolute zero |

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

### Updating

```bash
# Auto-detects whether aizo was installed via cargo or npm and updates it
aizo upgrade

# Preview the command without running it
aizo upgrade --dry-run

# Force a specific package manager
aizo upgrade --method cargo   # runs: cargo install aizo --force
aizo upgrade --method npm     # runs: npm install -g aizo-node@latest
```

### Configuration

Set env vars in `~/.aizo/.env` (user-wide) or `./.env` (per-project). Shell env always wins.

```bash
# Only AIZO_DB_PATH is needed for basic use (add/recall/top/show)
export AIZO_DB_PATH=~/.aizo/preferences.db
```

| Variable | Default | Description |
|---|---|---|
| `AIZO_DB_PATH` | `~/.aizo/preferences.db` | SQLite database path |

---

## CLI reference

```
aizo [--db <path>] <COMMAND>
```

| Command | Description |
|---|---|
| `recall [query]` | Keyword + score-range recall — **primary agent call** |
| `top [N]` | Top-N entries by effective weight (read-only, default 10) |
| `show` | Full profile sorted by effective weight (read-only) |
| `add <item> <reason>` | Manually add or update a preference |
| `update <item>` | Update fields on an existing entry (item, reason, score, keywords, scenarios) |
| `apply <id…>` | Mark recalled entries as actually used; refreshes decay with a 12-hour cooldown |
| `touch <item…>` | Reset decay clock by item label, subject to the same 12-hour cooldown |
| `remove <item…>` | Hard-remove an entry |
| `keywords` | List all stored keywords with entry counts |
| `scenarios` | List all scenarios with entry counts and configured keywords |
| `clear` | Wipe entire preference profile |
| `info` | DB path, score distribution, env config, decay settings |
| `config show/set-half-life/set-floor` | Get or set decay parameters |

**`recall` flags:**

| Flag | Description |
|---|---|
| `--type/-t <types>` | Score-range filter, comma-separated: `preference`, `style`, `habit`, `aversion`, `taboo` |
| `--limit/-l <N>` | Cap results after sorting by effective weight |
| `--scenario <name>` | Scenario-tagged recall + keyword expansion from `~/.aizo/scenarios.yaml` |
| `--min-score <N>` | Minimum `base_score` threshold (0.0–10.0); clamps band lower bounds |
| `--touch` | Refresh matched entries, subject to the 12-hour cooldown; recall is read-only by default |
| `--no-touch` | Deprecated no-op kept for older scripts |
| `--json` | Output raw JSON instead of human-readable text |

**`top` flags:** `--type/-t`, `--scenario`, `--json`. Read-only — never touches `last_seen`.

**`show` flags:** `--json` only. Read-only — never touches `last_seen`.

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

Use keywords (`--keywords` on add, or `aizo update --keywords`) to add any taxonomy you want.

### Examples

```bash
# Agent recalls preferences before generating
aizo top 5
aizo recall "code style"

# Scenario-aware recall for coding tasks (expands to ~10 coding keywords)
aizo recall --scenario coding --type preference,style,habit,taboo --limit 20

# Mark only the preferences the agent actually used
aizo apply 3 8 12

# Type-only recall (no keyword — returns all entries in that score range)
aizo recall --type taboo                        # all hard limits
aizo recall code --type preference --limit 10   # top coding preferences
aizo recall code --type preference,habit --limit 20  # multiple types

# Custom minimum score threshold
aizo recall --scenario coding --min-score 5.0 --limit 20

# Inspect full profile or top-N
aizo show
aizo top 20 --scenario coding --json

# Manual entries — score encodes sentiment
aizo add "concise code"     "Always asks for shorter implementations"  --score 9.0
aizo add "verbose comments" "Complained about over-documented code"    --score 1.5
aizo add "emojis in output" "Explicitly said never use emojis"         --score 0.5
aizo add "uses dark mode"   "Mentioned dark theme in every UI session" --score 5.0 --scenarios coding

# Update an existing entry
aizo update "concise code" --score 8.5 --keywords brevity,minimal,short
aizo update "verbose comments" --scenarios coding,writing

# Tune decay (default: half-life 30d, floor 0.1)
aizo config set-half-life 14
aizo config set-floor 0.05

# Optional length observability (does not affect effective_weight yet)
aizo config set-token-counting true

# Stats
aizo info
```

`set-token-counting true` stores an estimated `token_count` for each memory and
backfills existing rows. The estimate is observational only: it is returned in JSON
and shown in the web inspector, but it is not part of the weight formula.

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
  "touch_count": 2,
  "token_count": 18,
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
    last_seen   TEXT    NOT NULL,               -- resets decay clock on each reinforcement
    touch_count INTEGER NOT NULL DEFAULT 0,
    token_count INTEGER NOT NULL DEFAULT 0       -- estimated length, observational only
);
-- UNIQUE on LOWER(item)

CREATE TABLE decay_config (
    id              INTEGER PRIMARY KEY CHECK(id = 1),
    half_life_days  REAL    NOT NULL DEFAULT 30.0,
    floor           REAL    NOT NULL DEFAULT 0.1,
    token_counting_enabled INTEGER NOT NULL DEFAULT 0
);
```

---

## Agent integration

Any agent can call aizo as a subprocess — no embedding, no vector index, no runtime:

```python
import subprocess, json

def recall_scenario(scenario: str, min_score: float = 3.0) -> list[dict]:
    """Just-in-time recall for a specific task scenario."""
    return json.loads(subprocess.check_output(
        ["aizo", "recall", "--scenario", scenario,
         "--type", "preference,style,habit,taboo",
         "--min-score", str(min_score), "--limit", "20", "--json"]
    ))

def top_preferences(n: int = 10) -> list[dict]:
    return json.loads(subprocess.check_output(["aizo", "top", str(n), "--json"]))

def apply_preferences(ids: list[int]) -> None:
    """Call after the agent actually used specific recalled preferences."""
    if ids:
        subprocess.check_call(["aizo", "apply", *map(str, ids)])

# Just-in-time: before coding, recall coding-specific preferences
coding_prefs = recall_scenario("coding")
# Inject into session context — don't write to disk
context = f"[Coding preferences]\n{json.dumps(coding_prefs, indent=2)}"
# After generation, apply only the ids that shaped the response.
apply_preferences([p["id"] for p in coding_prefs[:3]])

# Before writing a document, recall writing preferences
writing_prefs = recall_scenario("writing")
```

Or configure `AIZO_DB_PATH` per-project to maintain separate profiles:

```bash
export AIZO_DB_PATH=./project-prefs.db
aizo show
```

### Just-in-time scenario recall

Not all preferences belong in persistent context files like `CLAUDE.md` or `MEMORY.md` —
those files would grow unboundedly. Instead, use **scenario-based recall** to pull relevant
preferences on demand, right when the agent receives a task:

```
agent receives task ──► classify into scenario ──► aizo recall --scenario <X>
                                                          │
                                                          ▼
                                               inject results into session context
                                                          │
                                                          ▼
                                               generate response with preferences applied
                                                          │
                                                          ▼
                                               aizo apply <used ids>
```

This keeps the agent's base context lean while giving it access to the full preference
profile. The pattern works like human memory: you don't pre-load every preference into
working memory — you recall the relevant ones when the situation calls for it.

**Example flow (coding task):**

```bash
# Agent classifies the user's request as a coding task, then recalls candidates:
aizo recall --scenario coding --type preference,style,habit,taboo --min-score 3.0 --limit 20 --json

# After generating, mark only the entries that were actually used:
aizo apply 3 8 12
```

**Example flow (writing task):**

```bash
# Agent classifies the user's request as a writing task, then:
aizo recall --scenario writing --type preference,style,taboo --limit 15 --json
```

**Creating scenario-specific entries.** When the user expresses a preference that only
applies to a certain context, tag it with that scenario so it surfaces only for relevant
tasks:

```bash
aizo add "no emojis in code" "Rejected emoji in a PR comment" --score 1.5
aizo update "no emojis in code" --scenarios coding,review

aizo add "use active voice" "Praised direct, active-voice writing" --score 8.5
aizo update "use active voice" --scenarios writing
```

The agent then only sees "no emojis in code" when coding — not when writing casual messages.
This scoped recall prevents preference leakage across unrelated task domains.

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
| 5 | About to generate on topic X | Classify task → `aizo recall --scenario <X> --min-score 3.0` → inject → `aizo apply <used ids>` | Sync recall before generation, apply after |
| 6 | Historical batch analysis | Agent LLM scans past sessions → `aizo add` new + `aizo apply`/`touch` confirmed | Scheduled, background |
| 7 | Daily cron job | Agent LLM scans logs → `aizo apply`/`touch` confirmed items | Scheduled, background |

**Key rules encoded in the skill:**
- Taboos always win over preferences in conflicts
- Silence (`recall` returning nothing) means no data, not neutral preference
- Never mention aizo to the user — it runs silently
- Use scenario recall for just-in-time context; don't dump everything into CLAUDE.md

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
