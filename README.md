# aizo 爱憎

**aizo** (爱憎, *ài zēng*, "love and hate") is a fast Rust CLI tool that builds a persistent preference profile for a user.  
It uses a flash LLM — `claude-haiku-4-5` — to analyze conversation sessions and extract what the user loves and hates: coding style preferences, workflow habits, opinions, and pet peeves.  
Preferences are stored in a local **SQLite** database and can be recalled instantly by any agent to personalize its behavior.

---

## How it works

```
session transcript
       │
       ▼
  flash LLM (claude-haiku-4-5)     ← cheap, fast, low-latency
       │  extracts loves / hates
       ▼
  SQLite (~/.aizo/preferences.db)
       │
       ▼
  aizo recall <query>  →  agent reads relevant prefs → personalizes responses
```

1. Feed session text to `aizo analyze` — the flash LLM extracts labelled preferences with confidence scores.
2. Preferences are merged into the SQLite database (higher-confidence entries win on conflict).
3. On the next session the agent calls `aizo recall <keyword>` to retrieve relevant preferences and adapt its tone, style, and suggestions.

---

## Installation

### From source (requires Rust ≥ 1.70)

```bash
git clone https://github.com/mmmarcinho/aizo
cd aizo
cargo build --release
# binary at ./target/release/aizo
cp target/release/aizo /usr/local/bin/aizo
```

Set your API key for the `analyze` command:

```bash
export ANTHROPIC_API_KEY=sk-ant-...
```

---

## CLI reference

```
USAGE
    aizo [--db <path>] <COMMAND>

COMMANDS
    analyze [file]              Analyze a session file (or stdin) with the flash LLM
    recall  <query>             Recall preferences matching a keyword  ← primary agent use-case
    show                        Print the full profile as JSON
    add love|hate <item> <reason…>   Manually record a preference
    remove love|hate <item…>    Remove a preference by label
    clear                       Wipe the entire profile
    info                        Show database path and counts

OPTIONS
    --db <path>                 Override database path (also: AIZO_DB_PATH env var)
```

### Examples

```bash
# Analyze a saved session log
aizo analyze ./session.txt

# Pipe from another tool
cat conversation.md | aizo analyze

# Fast keyword recall (what an agent calls before generating a response)
aizo recall "code style"
aizo recall "comments"

# Manually add a preference
aizo add love "dark mode"   "Always uses dark theme in every tool"
aizo add hate "long PRs"    "Prefers small, focused pull requests"

# Remove one
aizo remove hate "long PRs"

# Inspect everything
aizo show

# Stats
aizo info
```

### Custom database path

```bash
export AIZO_DB_PATH=/path/to/project/preferences.db
aizo show
```

---

## Preference record format

```json
{
  "id": 1,
  "category": "love",
  "item": "concise code",
  "reason": "Consistently asked for shorter implementations with no fluff.",
  "confidence": 0.92,
  "source": "analysis",
  "added_at": "2026-05-07T14:00:00+00:00"
}
```

| Field | Description |
|---|---|
| `category` | `"love"` or `"hate"` |
| `item` | Short reusable label — used for deduplication (case-insensitive) |
| `reason` | One-sentence explanation from the LLM or from you |
| `confidence` | 0.0–1.0; manual entries default to 1.0; higher confidence wins on conflict |
| `source` | `"analysis"` (from LLM) or `"manual"` (from CLI) |

---

## Database schema

```sql
CREATE TABLE preferences (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    category    TEXT    NOT NULL CHECK(category IN ('love','hate')),
    item        TEXT    NOT NULL,
    reason      TEXT    NOT NULL,
    confidence  REAL    NOT NULL DEFAULT 1.0,
    source      TEXT    NOT NULL DEFAULT 'manual',
    added_at    TEXT    NOT NULL
);
-- unique per (category, lower(item))

CREATE TABLE sessions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    analyzed_at     TEXT    NOT NULL,
    loves_extracted INTEGER NOT NULL DEFAULT 0,
    hates_extracted INTEGER NOT NULL DEFAULT 0
);
```

---

## Agent integration

Because `aizo` is a plain binary with machine-readable JSON output, any agent can call it as a subprocess:

```python
import subprocess, json

def recall(query: str) -> list[dict]:
    out = subprocess.check_output(["aizo", "recall", query])
    return json.loads(out)

prefs = recall("code style")
# inject into system prompt or planning context
```

Or from a shell hook / tool definition in Claude Code:

```json
{
  "tools": [{
    "name": "recall_preferences",
    "description": "Recall user loves/hates matching a keyword",
    "command": "aizo recall {query}"
  }]
}
```

---

## Development

```bash
cargo build          # debug
cargo build --release
cargo test
```

---

## License

MIT
