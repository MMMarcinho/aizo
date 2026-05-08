# aizo 爱憎

**aizo**（爱憎，*ài zēng*）是一个专为 AI 智能体设计的轻量、高性能偏好记忆系统，完全用 Rust 构建。

它模仿人类认知记忆的工作方式：不存储完整的对话记录，而是持续地从历史交互中**提取、量化、衰减、召回**用户稳定的偏好、厌恶、习惯、沟通风格和硬性边界。最终形成一个紧凑的、带数值权重的个性画像，任何智能体都能在毫秒内完成查询。

---

## 核心设计

```
对话记录
    │
    ▼
Flash LLM（claude-haiku-4-5）
    │  语义提取
    ▼
结构化词条  { 类别, 标签, 基础分数 0–10 }
    │  平滑合并
    ▼
SQLite（~/.aizo/preferences.db）
    │
    ▼
有效权重 = s · d(t)^α   （分数调制的衰减）
    │  关键词或 Top-N 召回
    ▼
智能体读取画像 → 个性化响应
```

### 评分公式

所有评分逻辑集中在 `src/scoring/mod.rs`。每条偏好词条在读取时，根据 `base_score` 和 `last_seen` 时间戳实时计算三个字段。

**第一步 — 衰减系数** $d(t)$

$$d(t) = \phi + (1 - \phi) \cdot e^{-\lambda t}, \quad \lambda = \frac{\ln 2}{t_{1/2}}$$

$t$ 为距上次触发的天数，$t_{1/2}$ 为配置的半衰期，$\phi$ 为衰减下限。

**第二步 — 分数指数** $\alpha$

$$\alpha = \frac{10 - s}{10}$$

分数越高 → $\alpha$ 越小 → 衰减影响越弱。分数为 10 时（$\alpha = 0$），完全不受时间影响；分数为 0 时（$\alpha = 1$），以最快速度衰减。

**第三步 — 有效权重** $w$

$$w = s \cdot d(t)^{\alpha}$$

展开为单一表达式：

$$\boxed{w = s \cdot \left[\phi + (1-\phi) \cdot e^{-\lambda t}\right]^{\frac{10-s}{10}}}$$

**直觉解释：你越爱的东西，越不容易被遗忘；无所谓的习惯，慢慢就淡了。**

**边界行为**

| 分数 $s$ | $\alpha$ | 衰减效果 | 含义 |
|---|---|---|---|
| 10 | 0.0 | 无 — $d^0 = 1$ | 核心价值观，永不消退 |
| 7  | 0.3 | 轻微 | 强烈偏好，缓慢消退 |
| 5  | 0.5 | 中等 | 中性习惯，半速消退 |
| 1  | 0.9 | 接近完全 | 弱厌恶，快速消退 |
| 0  | 1.0 | 完全 | $w = 0$，始终为零 |

词条**永远不会因衰减而被删除**——它们会沉降至下限，以弱长期记忆的形式保留。使用 `--type taboo` 可无视有效权重，显式召回禁忌词条。

### 分数量表（0–10）

| 分数 | 含义 |
|---|---|
| 0 | 绝对禁忌 / 硬性拒绝 |
| 1–3 | 明确厌恶 |
| 4–6 | 中性倾向 / 弱习惯 |
| 7–9 | 明确偏好 |
| 10 | 强烈、一贯、高优先级的喜爱 |

### 分数平滑

同一词条在多次会话中被提及时：

```
新基础分数 = 旧基础分数 × 0.4 + 新提取分数 × 0.6
```

`last_seen` 每次都会刷新，重置衰减时钟。

---

## 安装

### 从源码编译（需要 Rust ≥ 1.70）

```bash
git clone https://github.com/mmmarcinho/aizo
cd aizo
cargo build --release
cp target/release/aizo /usr/local/bin/aizo
```

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # analyze 命令必需
```

---

## 命令参考

```
aizo [--db <路径>] <命令>
```

| 命令 | 说明 |
|---|---|
| `analyze [文件]` | 用 Flash LLM 分析会话文件（或标准输入） |
| `recall <关键词>` | 按有效权重排序的关键词召回 — **智能体核心调用** |
| `top [N]` | 按有效权重排列的前 N 条（默认 10） |
| `show` | 以 JSON 输出完整画像，按有效权重排序 |
| `add <类别> <标签> <原因…>` | 手动添加或更新一条偏好 |
| `touch <类别> <标签…>` | 重置衰减时钟，不修改分数 |
| `remove <类别> <标签…>` | 硬删除一条词条 |
| `clear` | 清空整个画像和会话历史 |
| `info` | 显示数据库路径、各类别数量、衰减配置 |
| `config show` | 显示衰减配置 |
| `config set-half-life <天数>` | 设置衰减半衰期 |
| `config set-floor <0.0–1.0>` | 设置衰减下限 |

### 类别说明

| 类别 | 别名 | 默认分数 | 含义 |
|---|---|---|---|
| `preference` | `love` | 9.0 | 一贯的喜好和优先项 |
| `aversion` | `hate` | 1.0 | 厌恶和反感点 |
| `habit` | — | 5.0 | 行为模式，中性 |
| `style` | — | 8.0 | 沟通和格式偏好 |
| `taboo` | — | 0.5 | 硬性边界，绝对禁止 |

### 使用示例

```bash
# 分析一段对话记录
aizo analyze ./chat.txt
cat conversation.md | aizo analyze

# 智能体生成前召回偏好
aizo top 5
aizo recall "代码风格"

# 查看完整画像
aizo show

# 手动录入
aizo add love "简洁的代码"    "总是要求更短的实现"
aizo add hate "冗长注释"      "多次抱怨过度文档化的代码"
aizo add taboo "输出中使用表情符号" "明确说过永远不要用"
aizo add habit "使用深色模式"  "每次 UI 会话都提到深色主题"
aizo add style "简短命名"     "一贯选择短变量名"

# 调整衰减参数（默认：半衰期 30 天，下限 0.1）
aizo config set-half-life 14
aizo config set-floor 0.05

# 查看统计
aizo info
```

---

## 词条格式

```json
{
  "id": 1,
  "category": "preference",
  "item": "简洁的代码",
  "reason": "总是要求更短的实现，不喜欢多余内容。",
  "keywords": ["brevity", "minimal", "short", "lean"],
  "base_score": 9.0,
  "source": "analysis",
  "added_at": "2026-05-07T14:00:00+00:00",
  "last_seen": "2026-05-07T15:30:00+00:00",
  "score_exponent": 0.1,
  "decay_coefficient": 0.87,
  "effective_weight": 8.78
}
```

---

## 数据库结构

```sql
CREATE TABLE preferences (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    category    TEXT    NOT NULL
        CHECK(category IN ('preference','aversion','habit','style','taboo')),
    item        TEXT    NOT NULL,
    reason      TEXT    NOT NULL,
    keywords    TEXT    NOT NULL DEFAULT '',  -- 逗号分隔的同义词标签
    base_score  REAL    NOT NULL DEFAULT 5.0, -- 0-10
    source      TEXT    NOT NULL DEFAULT 'manual',
    added_at    TEXT    NOT NULL,
    last_seen   TEXT    NOT NULL              -- 每次强化时重置衰减时钟
);
-- UNIQUE 约束：(category, LOWER(item))

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

## 智能体集成

任何智能体都可以将 aizo 作为子进程调用——无需向量索引，无需额外运行时：

```python
import subprocess, json

def top_preferences(n: int = 10) -> list[dict]:
    return json.loads(subprocess.check_output(["aizo", "top", str(n)]))

def recall(query: str, kind: str | None = None) -> list[dict]:
    cmd = ["aizo", "recall", query]
    if kind:
        cmd += ["--type", kind]
    return json.loads(subprocess.check_output(cmd))

# 生成前注入系统提示
prefs = top_preferences(10)
system = f"用户偏好：\n{json.dumps(prefs, indent=2, ensure_ascii=False)}\n\n{base_system}"
```

通过环境变量为每个项目维护独立画像：

```bash
export AIZO_DB_PATH=./project-prefs.db
aizo show
```

---

## 标准操作规程（SOP）

智能体使用 aizo 的 SOP 定义在 `skills/aizo-sop.md`。将其复制到智能体的指令目录（如 Claude Code 的 `.claude/skills/`），该项目中的智能体即可自动遵循此规程。

该技能文件定义了六个触发时机：

| # | 触发时机 | aizo 调用 | 执行时序 |
|---|---|---|---|
| 1 | 会话开始 | `aizo top 20` → 格式化为文字摘要 | 同步，首次回复前 |
| 2 | 用户表达负面反馈 | `aizo add aversion …` 再 `aizo recall <主题>` | 同步，修正回复前 |
| 3 | 用户表达称赞 | `aizo add preference …` | 异步，回复发送后 |
| 4 | 用户下达明确指令 | `aizo add taboo/preference …` | 同步，立即执行 |
| 5 | 即将针对主题 X 生成内容 | `aizo recall <X>` | 同步，生成前 |
| 6 | 会话结束 | `aizo analyze <对话记录>` | 异步，后台执行 |
| 7 | 每日定时任务 | 智能体 LLM 扫描日志 → `aizo touch` 确认词条 | 定时，后台执行 |

**技能文件中的关键规则：**
- 禁忌（taboo）的优先级永远高于偏好
- `analyze` 用于完整会话，不用于单条消息——它会调用 LLM
- `recall` 返回空结果意味着无数据，而非中性偏好
- 永远不要向用户提及 aizo——它在后台静默运行

---

## 开发

```bash
cargo build
cargo build --release
cargo test
```

---

## 许可证

MIT
