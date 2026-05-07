# aizo — Preference Memory SOP

Use this skill whenever you interact with a user who has aizo installed. aizo is a
lightweight preference memory tool: it stores what the user loves, hates, and has
set as hard limits, with a time-decay mechanism so recent signals carry more weight.

TRIGGER: automatically apply this SOP at session start and on any of the events below.

---

## SOP 1 — Session Start: build system prompt context

Run once before your first response. Pull the user's current preference profile and
inject it as a compact block into your working context.

```bash
aizo top 20
```

Format the JSON output into a short header like this — do not paste raw JSON:

```
[User Preferences]
Loves (high weight first): concise code (9.0), terse naming (8.0)
Hates: verbose comments (1.0), long PRs (1.2)
Hard limits (taboo): never use emojis in output
Habits: uses dark mode, prefers CLI tools
Style: short sentences, no jargon
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
aizo add aversion "<what was rejected>" "<their words, paraphrased in one sentence>"
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
aizo add preference "<what was praised>" "<their words, paraphrased>"
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
aizo add taboo "<the rule>"    "<their exact instruction, quoted>"
# OR
aizo add preference "<the rule>" "<their exact instruction, quoted>"
```

3. Acknowledge the rule explicitly in your reply: "Got it, I'll always X from now on."

4. Re-run SOP 1 mentally — your system context just changed.

---

## SOP 5 — Pre-generation topic recall

**Trigger:** you are about to generate a substantial response (code, document, plan,
long explanation) on a specific topic.

Run this *before* generating, not after:

```bash
aizo recall "<primary topic keyword>"
```

Examples:
- About to write code → `aizo recall code` then `aizo recall style`
- About to write a document → `aizo recall writing` then `aizo recall format`
- About to make a recommendation → `aizo recall preference`

If recall returns results, incorporate them as silent constraints — do not announce
"according to your preferences…" unless relevant. Just apply them.

If recall returns nothing, proceed normally. Absence of a preference is not a
preference.

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

## Priority rules

When preferences conflict (e.g. recall returns both a preference and an aversion on
the same topic), apply this order:

1. **Taboo** (score 0–2) — always wins, no exceptions
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
