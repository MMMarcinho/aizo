# aizo — Preference Memory SOP

Use this skill whenever you interact with a user who has aizo installed. aizo is a
lightweight preference memory tool: it stores what the user loves, hates, and has
set as hard limits, with a time-decay mechanism so recent signals carry more weight.

## How aizo fits into the agent ecosystem

aizo operates in two complementary loops. Understand which loop you are in:

```
╔══════════════════════════════════════════════════════════════════════╗
║  Loop 1 — In-session  (you are here during a live conversation)      ║
╚══════════════════════════════════════════════════════════════════════╝

   user ──► agent (you) ──── aizo add ───────────────────┐
                                                          ▼
                          CLAUDE.md ◄── contributes ── local SQLite


╔══════════════════════════════════════════════════════════════════════╗
║  Loop 2 — Background  (cron job, runs outside live sessions)         ║
╚══════════════════════════════════════════════════════════════════════╝

   accumulated sessions ── aizo analyze ─────────────────┐
                                                          ▼
   USER.md, SOUL.md, IDENTITY.md … ◄── contributes ── local SQLite
```

**Loop 1 (SOPs 1–6):** you run live. Detect preference signals, write them with
`aizo add`, recall before generating, and batch-analyze the session transcript at the end.
The profile flows back into `CLAUDE.md` so the next session starts informed.

**Loop 2 (SOP 7):** a scheduled cron job processes accumulated session transcripts with
`aizo analyze`. The enriched profile is then used to maintain richer identity files
(`USER.md`, `SOUL.md`, `IDENTITY.md`, etc.) that persist the user's evolving persona
across all agents and tools.

The SOPs below cover both loops. SOPs 1–6 are for live sessions; SOP 7 is for the cron job.

TRIGGER: automatically apply this SOP at session start and on any of the events below.

---

## SOP 1 — Session Start: build system prompt context

Run once before your first response. Pull the user's current preference profile and
inject it as a compact block into your working context.

```bash
aizo top 20 --json
```

Format the JSON output into a short prose header — do not paste raw JSON:

```
[User Preferences]
Strong likes (weight ≥ 7): concise code (9.0), terse naming (8.0)
Dislikes (weight ≤ 4): verbose comments (1.0), long PRs (1.2)
Hard limits (weight ≤ 1.5): never use emojis in output
Neutral habits: uses dark mode, prefers CLI tools
```

Inject this block at the top of your system context. Re-run at the start of each
new conversation — do not cache across sessions because effective weights change
as time passes.

---

## SOP 2 — Negative feedback: record + adjust reply

**Trigger:** user expresses dissatisfaction. Signals include:
- Explicit: "too long", "wrong style", "I hate this", "stop doing X", "no"
- Implicit: correcting your output, rewriting your suggestion, dismissing without using it

**Steps:**

1. Identify the specific behaviour they rejected (1 short label).
2. Write it immediately — do not wait:
```bash
aizo add "<what was rejected>" "<their words, paraphrased in one sentence>" --score 2.0
```
3. Recall relevant preferences for the corrected reply:
```bash
aizo recall "<topic of current task>" --type aversion
aizo recall "<topic of current task>" --type taboo
```
4. Generate the corrected reply. Explicitly avoid the rejected behaviour and anything
   related returned by recall.
5. Do not explain that you updated the profile unless asked — just fix the reply.

---

## SOP 3 — Positive feedback: reinforce

**Trigger:** user explicitly praises something. Signals include:
- "exactly", "perfect", "yes", "keep doing this", "love this", "much better"

**Steps:**

1. Identify what specifically they praised.
2. Reinforce it:
```bash
aizo add "<what was praised>" "<their words, paraphrased>" --score 9.0
```

Do this *after* sending the reply, not before — positive reinforcement is async.
Keep the score at its default (9.0) unless the praise was extraordinary ("this is
exactly how I always want you to respond"), in which case note the strength in the
reason field.

---

## SOP 4 — Explicit instruction: immediate hard write

**Trigger:** user states a rule directly. Signals include:
- "always do X", "never do Y", "from now on X", "I want you to always X"
- "don't ever", "make sure you always", "rule:"

This is different from inferred preferences — these are commands, not signals.

**Steps:**

1. Classify: is this a hard limit (taboo) or a strong preference?
   - "never", "don't ever", "absolutely not" → taboo (score ~0)
   - "always", "I want you to always" → preference (score 10)

2. Write immediately and synchronously, before generating your reply:
```bash
# Hard limit ("never", "don't ever") — score near 0
aizo add "<the rule>" "<their exact instruction, quoted>" --score 0.5
# Strong preference ("always", "I want you to always") — score 10
aizo add "<the rule>" "<their exact instruction, quoted>" --score 10.0
```

3. Acknowledge the rule explicitly in your reply: "Got it, I'll always X from now on."

4. Re-run SOP 1 mentally — your system context just changed.

---

## SOP 5 — Pre-generation topic recall (just-in-time scenario recall)

**Trigger:** you are about to generate a substantial response (code, document, plan,
long explanation) on a specific topic — OR you have just received a user instruction
and are classifying it before acting.

**Core idea:** not all preferences should be pre-loaded into `CLAUDE.md` or `MEMORY.md`.
Those files would grow without bound. Instead, the full preference profile lives in
aizo's database, and you pull in only what is relevant to the current task — just like
human working memory.

### Step 1 — Classify the task into a scenario

When a user gives you a task, first determine which scenario it belongs to. Common
scenarios and their typical keywords (defined in `~/.aizo/scenarios.yaml`):

| Scenario | When to use | Typical keywords |
|---|---|---|
| `coding` | Writing, reviewing, or discussing code | coding, codex, repo, test, verification, tnpm |
| `writing` | Writing prose, docs, emails, messages | writing, prose, docs, tone, style |
| `communication` | How to interact with the user | communication, tone, brevity, detail, emoji |
| `design` | UI/UX, visual design, layout | design, ui, ux, layout, visual, color |
| `review` | Code review, PR feedback | review, pr, diff, feedback, approve |

You can define additional scenarios in `~/.aizo/scenarios.yaml`. If no scenario matches
the task domain, skip scenario-based recall and use a plain keyword recall instead.

### Step 2 — Recall preferences for that scenario

Run this *before* generating, not after:

```bash
# Primary: scenario-based recall — pulls entries tagged with the scenario
# plus entries matching the scenario's configured keywords.
aizo recall --scenario coding --type preference,style,habit,taboo --limit 20

# For stricter filtering, add --min-score to suppress weak/irrelevant entries:
aizo recall --scenario coding --type preference,style,habit,taboo --min-score 3.0 --limit 20

# Supplement with a targeted keyword recall for the specific subtopic:
aizo recall "<primary topic keyword>" --type preference,style,taboo

# Always check for hard limits regardless of scenario:
aizo recall --type taboo
```

### Step 3 — Inject results into session context

Format the recall output into a compact block and hold it in your working context for
the duration of this task. Do not write it to disk — it is session-scoped:

```
[Relevant Preferences — coding scenario]
Strong likes: concise code (9.0), terse naming (8.0)
Style: prefers functional patterns (7.5)
Habits: uses CLI tools (5.0)
Hard limits: never use emojis in code (0.5)
```

### Step 4 — Apply silently

Incorporate the recalled preferences as silent constraints. Do NOT announce "according
to your preferences…" or "based on your aizo profile…". Just apply them.

### Step 5 — Keep it fresh

Preferences evolve. Re-run scenario recall when the task domain changes (e.g., switching
from coding to writing). Do NOT cache scenario recall results across task domains — the
relevant preferences differ.

### Recall strategies by task scope

| Task scope | Recall strategy |
|---|---|
| Quick question, no generation | Skip — no recall needed |
| Small generation (single function, short reply) | `aizo recall <keyword> --type preference,taboo` |
| Substantial generation (PR, document, design) | `aizo recall --scenario <X> --type preference,style,habit,taboo --min-score 3.0 --limit 20` |
| High-stakes or user explicitly picky | Above + `--min-score 1.5` (lower threshold to catch aversions too) |
| New topic, no scenario defined | `aizo recall <topic> --type preference,style,taboo` |
| Any task | `aizo recall --type taboo` (always check hard limits) |

### What if recall returns nothing?

Absence of a preference is not a preference. Proceed normally with your default
behaviour — the user hasn't expressed anything about this scenario yet.

### Important: scenario-scoped vs global

Preferences tagged with a scenario (`--scenario coding`) only surface when you recall
that scenario. Preferences without a scenario tag appear in plain keyword recall but
NOT in scenario-based recall (which requires either the scenario tag or a keyword match
from the scenario config). This is intentional: it prevents preference leakage across
unrelated task domains.

When the user expresses a context-specific preference, tag it with the relevant scenario:

```bash
# This preference only applies to coding — tag it accordingly
aizo add "no emojis in code" "Rejected emoji in a PR comment" --score 1.5
aizo tag "no emojis in code" coding review
```

Now it surfaces when recalling `coding` or `review`, but not when recalling `writing`.

---

## SOP 6 — End of session: batch analysis

**Trigger:** the conversation is concluding (user says goodbye, task is complete,
long silence, explicit sign-off).

Collect the full session text and run:

```bash
aizo analyze <session-file>
# OR pipe from your session buffer:
echo "<full session text>" | aizo analyze
```

This captures implicit signals that were not obvious enough to trigger SOPs 2–4 in
real time. It is async — run it after the session ends, not during.

Do not run this mid-session: the flash LLM extracts preferences from the whole arc
of a conversation, and partial sessions produce noisy results.

---

## SOP 7 — Daily cron scan: refresh confirmed memories

**Trigger:** a scheduled cron job, not a live session event. Recommended frequency: once per day.

**Problem this solves:** `analyze` only discovers *new* preferences. Existing memories whose
`last_seen` is never refreshed will slowly decay — even if the user demonstrates them every day.
The cron scan confirms which existing memories are still active and resets their decay clocks.

**The agent's responsibility** (the CLI has no LLM — this logic runs on your side):

1. Collect the past day's session transcripts or recent interaction logs.

2. Load the current preference list:
```bash
aizo show
```

3. Ask your LLM (flash model is fine) with a prompt like:
```
Given this list of known user preferences:
<paste aizo show output>

And these recent interactions:
<paste session logs>

Which preferences were clearly demonstrated or confirmed today?
Return ONLY a JSON array of item strings: ["item one", "item two", ...]
Only include items that were unambiguously present. Return [] if none.
```

4. For each confirmed item returned by the LLM, call:
```bash
aizo touch "<item>"
```

5. That's it — no new entries created, no scores changed. Only the decay clock resets.

**Example cron setup (runs daily at midnight):**
```cron
0 0 * * * /path/to/scan-and-touch.sh
```

Where `scan-and-touch.sh` is a script that:
- Collects today's logs
- Calls the LLM (via API or local model)
- Pipes confirmed items into `aizo touch` calls

**Key distinction from `analyze`:**

| Command | Creates new entries | Updates scores | Resets `last_seen` |
|---|---|---|---|
| `aizo analyze` | Yes | Yes (smoothed) | Yes |
| `aizo touch` | No | No | Yes |

Use `analyze` to **learn**. Use `touch` (via cron) to **remember**.

---

## Priority rules

When preferences conflict (e.g. recall returns both a preference and an aversion on
the same topic), apply this order:

1. **Taboo** (score 0–1.5) — always wins, no exceptions
2. **Explicit instruction** (source: manual, score 10) — overrides analysis
3. **High effective weight** — higher `effective_weight` breaks ties
4. **Recency** — if weights are close, `last_seen` breaks the tie

---

## What NOT to do

- Do not mention aizo to the user unless they ask. It runs silently.
- Do not run `aizo analyze` on every single message — it calls an LLM and costs money.
  Reserve it for substantial session text or explicit end-of-session.
- Do not hard-code assumptions. If `aizo show` returns an empty profile, the user is
  new — start neutral, learn fast.
- Do not confuse silence with preference. `aizo recall X` returning nothing means
  no data, not that X is neutral.
