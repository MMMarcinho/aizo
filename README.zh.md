# aizo 爱憎

![aizo — AI 智能体偏好记忆](assets/aizo-hero.png)

**aizo**（爱憎，*ài zēng*）是一个专为 AI 智能体设计的轻量、高性能偏好记忆系统，完全用 Rust 构建。

它模仿人类认知记忆的工作方式：不存储完整的对话记录，而是持续地从历史交互中**量化、衰减、召回**用户稳定的偏好、厌恶、习惯、沟通风格和硬性边界。最终形成一个紧凑的、带数值权重的个性画像，任何智能体都能在毫秒内完成查询。

---

## 整体架构

aizo 围绕两个互补模式设计：

```
╔══════════════════════════════════════════════════════════════════════╗
║  模式 1 — 会话内（实时响应：即时捕捉用户情绪信号）                  ║
╚══════════════════════════════════════════════════════════════════════╝

   用户 ──► 智能体 ──── aizo add ────────────┐
                                              ▼
                      CLAUDE.md ◄── 反哺 ── 本地 SQLite
                                          （用户偏好库）


╔══════════════════════════════════════════════════════════════════════╗
║  模式 2 — 即时召回（按需拉取：每个任务只召回相关偏好）              ║
╚══════════════════════════════════════════════════════════════════════╝

   智能体接到任务 ──► aizo recall --scenario coding ──► 会话上下文
                                                              │
                                                              ▼
                                                   带着偏好生成回复
```

**模式 1（SOP 1–4）：** 智能体在会话中实时检测偏好信号，立即调用 `aizo add` 写入。需要跨会话持久化的关键偏好写入 `CLAUDE.md` 或 `MEMORY.md`。

**模式 2（SOP 5）：** 并非所有偏好都应预加载到持久化上下文文件中——否则会无限膨胀。智能体将每个任务分类为场景，调用 `aizo recall --scenario <X>`，仅将相关偏好注入当前会话。这就像人类的"工作记忆"——用时才从长期记忆中调取。

历史会话的批量分析（发现新偏好）请参见 `skills/aizo-sop.md` 中的 SOP 6——这是技能层面的方案，而非 aizo 内置命令。

---

## 核心设计

```
智能体观察（赞扬、抱怨、规则、习惯）
       │
       ▼
  aizo add  { 标签, 基础分数 0–10, 关键词, 场景 }
       │  平滑合并（旧×0.4 + 新×0.6）
       ▼
  SQLite（~/.aizo/preferences.db）
       │
       ▼
  有效权重 = s · d(t)^α   （分数调制的衰减）
       │  关键词 / 分数区间 / 场景召回
       ▼
  智能体读取画像 → 个性化响应
```

### 评分公式

所有评分逻辑集中在 `src/scoring/mod.rs`。每条偏好词条在读取时，根据 `base_score` 和 `last_seen` 时间戳实时计算三个字段。

**第一步 — 衰减系数 d(t)**

$$d(t) = \phi + (1 - \phi) \cdot e^{-\lambda t}, \quad \lambda = \frac{\ln 2}{t_{1/2}}$$

其中 t 为距上次触发的天数，t½（半衰期）为配置的半衰期天数，φ（衰减下限）防止权重归零。

**第二步 — 分数指数 α**

$$\alpha = \frac{10 - s}{10}$$

分数越高 → α 越小 → 衰减影响越弱。分数为 10 时（α = 0），完全不受时间影响；分数为 0 时（α = 1），以最快速度衰减。

**第三步 — 有效权重 w**

$$w = s \cdot d(t)^{\alpha}$$

展开为单一表达式：

$$\boxed{w = s \cdot \left[\phi + (1-\phi) \cdot e^{-\lambda t}\right]^{\frac{10-s}{10}}}$$

**直觉解释：你越爱的东西，越不容易被遗忘；无所谓的习惯，慢慢就淡了。**

**边界行为**

| 分数 s | α | 衰减效果 | 含义 |
|---|---|---|---|
| 10 | 0.0 | 无（d⁰ = 1） | 核心价值观，永不消退 |
| 7  | 0.3 | 轻微 | 强烈偏好，缓慢消退 |
| 5  | 0.5 | 中等 | 中性习惯，半速消退 |
| 1  | 0.9 | 接近完全 | 弱厌恶，快速消退 |
| 0  | 1.0 | 完全（w = 0） | 始终为零 |

词条**永远不会因衰减而被删除**——它们会沉降至下限，以弱长期记忆的形式保留。

### 分数量表（0–10）

没有"类别"字段。`base_score` 是唯一的情感维度，也是 `--score-band` 过滤的依据：

| 分数 | 含义 | `--score-band` 别名 |
|---|---|---|
| 0–1.5 | 硬性边界 / 绝对禁忌 | `taboo` |
| 1.6–4 | 明确厌恶 | `aversion` |
| 4–6.5 | 中性倾向 / 弱习惯 | `habit` |
| 6.5–10 | 风格 / 沟通偏好 | `style` |
| 7–10 | 明确偏好 | `preference` |

在 `recall` 和 `top` 中使用 `--score-band` 按分数范围过滤，逗号分隔可多选：

```bash
aizo recall code --score-band preference,habit,style,taboo
aizo recall --score-band taboo            # 列出所有硬性边界
aizo top 5 --score-band preference
```

### 分数平滑

同一词条在多次会话中被提及时：

```
新基础分数 = 旧基础分数 × 0.4 + 新提取分数 × 0.6
```

`last_seen` 每次都会刷新，重置衰减时钟。

---

## 安装

```bash
# Cargo（推荐）
cargo install aizo

# npm / npx
npm install -g aizo
npx aizo top 10

# 从源码编译（需要 Rust ≥ 1.70）
git clone https://github.com/mmmarcinho/aizo
cd aizo && cargo build --release
cp target/release/aizo /usr/local/bin/aizo
```

### 配置

在 `~/.aizo/.env`（用户级）或 `./.env`（项目级）中设置环境变量。Shell 环境变量优先级最高。

```bash
# 基础使用（add/recall/top/show）只需要 AIZO_DB_PATH
export AIZO_DB_PATH=~/.aizo/preferences.db
```

| 变量 | 默认值 | 说明 |
|---|---|---|
| `AIZO_DB_PATH` | `~/.aizo/preferences.db` | SQLite 数据库路径 |

---

## 命令参考

```
aizo [--db <路径>] <命令>
```

| 命令 | 说明 |
|---|---|
| `recall [查询]` | 关键词 + 分数范围召回 — **智能体核心调用** |
| `top [N]` | 按有效权重排列的前 N 条（只读，默认 10） |
| `show` | 输出完整画像，按有效权重排序（只读） |
| `add <标签> <原因>` | 手动添加或更新一条偏好 |
| `update <标签>` | 更新已有词条的字段（标签、原因、分数、关键词、场景） |
| `apply <id…>` | 标记召回词条已被实际采用；按 12 小时冷却刷新衰减时钟 |
| `touch <标签…>` | 按标签重置衰减时钟，同样受 12 小时冷却限制 |
| `remove <标签…>` | 硬删除一条词条 |
| `keywords` | 列出所有已存储的关键词及词条数 |
| `scenarios` | 列出所有场景及词条数、配置关键词 |
| `clear` | 清空整个偏好画像 |
| `info` | 显示数据库路径、分数分布、衰减配置 |
| `config show/set-half-life/set-floor` | 查看或设置衰减参数 |

**`recall` 标志：**

| 标志 | 说明 |
|---|---|
| `--score-band <区间>` | 分数范围过滤，逗号分隔：`preference`、`style`、`habit`、`aversion`、`taboo` |
| `--type/-t <类型>` | `--score-band` 的别名（已废弃） |
| `--limit/-l <N>` | 按有效权重排序后限制返回数量 |
| `--scenario <名称>` | 场景召回 + 关键词扩展（从 `~/.aizo/scenarios.yaml` 读取） |
| `--min-score <N>` | 最低 `base_score` 阈值（0.0–10.0）；会抬高 score-band 的下界 |
| `--touch` | 刷新匹配词条，受 12 小时冷却限制；`recall` 默认只读 |
| `--no-touch` | 已废弃的兼容参数；默认就是不 touch |
| `--json` | 输出原始 JSON（供程序调用） |

**`top` 标志：** `--score-band/-t`、`--scenario`、`--json`。只读——不会刷新 `last_seen`。

**`show` 标志：** 仅 `--json`。只读——不会刷新 `last_seen`。

### 使用示例

```bash
# 智能体生成前召回偏好
aizo top 5
aizo recall "代码风格"

# 场景化召回（coding 自动展开为约 10 个相关关键词）
aizo recall --scenario coding --score-band preference,style,habit,taboo --limit 20

# 生成后只标记真正用到的偏好
aizo apply 3 8 12

# 仅按类型召回（无需关键词）
aizo recall --score-band taboo                       # 所有硬性边界
aizo recall code --score-band preference --limit 10   # 前 10 条编码偏好
aizo recall code --score-band preference,habit --limit 20  # 多类型

# 自定义最低分数阈值
aizo recall --scenario coding --min-score 5.0 --limit 20

# 查看完整画像或 Top-N
aizo show
aizo top 20 --scenario coding --json

# 手动录入 — 分数即情感
aizo add "简洁的代码"     "总是要求更短的实现"            --score 9.0
aizo add "冗长注释"       "多次抱怨过度文档化的代码"      --score 1.5
aizo add "输出中使用表情" "明确说过永远不要用"            --score 0.5
aizo add "使用深色模式"   "每次 UI 会话都提到深色主题"    --score 5.0 --scenarios coding

# 更新已有词条
aizo update "简洁的代码" --score 8.5 --keywords 简洁,简短,精炼
aizo update "冗长注释" --scenarios coding,writing

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
  "item": "简洁的代码",
  "reason": "总是要求更短的实现，不喜欢多余内容。",
  "keywords": ["简洁", "简短", "精炼"],
  "scenarios": ["coding"],
  "base_score": 9.0,
  "source": "manual",
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
    item        TEXT    NOT NULL,
    reason      TEXT    NOT NULL,
    keywords    TEXT    NOT NULL DEFAULT '',    -- 逗号分隔的同义词标签
    base_score  REAL    NOT NULL DEFAULT 5.0,   -- 0-10
    source      TEXT    NOT NULL DEFAULT 'manual',
    added_at    TEXT    NOT NULL,
    last_seen   TEXT    NOT NULL                -- 每次强化时重置衰减时钟
);
-- UNIQUE 约束：LOWER(item)

CREATE TABLE decay_config (
    id              INTEGER PRIMARY KEY CHECK(id = 1),
    half_life_days  REAL    NOT NULL DEFAULT 30.0,
    floor           REAL    NOT NULL DEFAULT 0.1
);

CREATE TABLE preference_scenarios (
    preference_id INTEGER NOT NULL,
    scenario      TEXT    NOT NULL,
    PRIMARY KEY (preference_id, scenario),
    FOREIGN KEY (preference_id) REFERENCES preferences(id) ON DELETE CASCADE
);
```

---

## 智能体集成

任何智能体都可以将 aizo 作为子进程调用——无需嵌入模型、无需向量索引、无需额外运行时：

```python
import subprocess, json

def recall_scenario(scenario: str, min_score: float = 3.0) -> list[dict]:
    """即时召回：根据任务场景拉取相关偏好。"""
    return json.loads(subprocess.check_output(
        ["aizo", "recall", "--scenario", scenario,
         "--score-band", "preference,style,habit,taboo",
         "--min-score", str(min_score), "--limit", "20", "--json"]
    ))

def top_preferences(n: int = 10) -> list[dict]:
    return json.loads(subprocess.check_output(["aizo", "top", str(n), "--json"]))

def apply_preferences(ids: list[int]) -> None:
    """生成后标记实际用到的偏好。"""
    if ids:
        subprocess.check_call(["aizo", "apply", *map(str, ids)])

# 即时召回：写代码前拉取编程相关偏好
coding_prefs = recall_scenario("coding")
# 注入到会话上下文——不写磁盘
context = f"[编程偏好]\n{json.dumps(coding_prefs, indent=2, ensure_ascii=False)}"
# 生成后只 apply 真正影响输出的词条 id
apply_preferences([p["id"] for p in coding_prefs[:3]])

# 写文档前拉取写作相关偏好
writing_prefs = recall_scenario("writing")
```

通过环境变量为每个项目维护独立画像：

```bash
export AIZO_DB_PATH=./project-prefs.db
aizo show
```

### 即时场景召回

并非所有偏好都应该写入 `CLAUDE.md` 或 `MEMORY.md`——那样会无限膨胀。取而代之，使用**场景召回**按需拉取相关偏好：

```
智能体接到任务 ──► 分类到场景 ──► aizo recall --scenario <X>
                                        │
                                        ▼
                             注入结果到会话上下文
                                        │
                                        ▼
                             带着偏好生成回复
                                        │
                                        ▼
                             aizo apply <用到的 id>
```

这保持了基础上下文的精简，同时让智能体能访问完整的偏好画像。模式类似人类记忆：你不会把每一条偏好都预加载到工作记忆中——只在需要时才想起。

**给词条打场景标签：**

```bash
# 这条偏好只适用于编程场景——打上对应标签
aizo add "代码中不用表情" "在 PR 评论中拒绝了表情符号" --score 1.5
aizo update "代码中不用表情" --scenarios coding,review

# 这条偏好只适用于写作场景
aizo add "使用主动语态" "夸赞了直接的主动语态写作" --score 8.5
aizo update "使用主动语态" --scenarios writing
```

智能体在编程时只会看到"代码中不用表情"，写消息时则不会——场景隔离防止偏好泄露到无关任务领域。

---

## 标准操作规程（SOP）

智能体使用 aizo 的 SOP 定义在 `skills/aizo-sop.md`。将其复制到智能体的指令目录（如 Claude Code 的 `.claude/skills/`），该项目中的智能体即可自动遵循此规程。

该技能文件定义了七个触发时机：

| # | 触发时机 | aizo 调用 | 执行时序 |
|---|---|---|---|
| 1 | 会话开始 | `aizo top 20` → 格式化为文字摘要 | 同步，首次回复前 |
| 2 | 用户表达负面反馈 | `aizo add … --score 1.5` 再 `aizo recall <主题>` | 同步，修正回复前 |
| 3 | 用户表达称赞 | `aizo add … --score 9.0` | 异步，回复发送后 |
| 4 | 用户下达明确指令 | `aizo add … --score 0.5` 或 `--score 10` | 同步，立即执行 |
| 5 | 即将针对主题 X 生成内容 | 分类任务 → `aizo recall --scenario <X> --min-score 3.0` → 注入 → `aizo apply <用到的 id>` | 生成前召回，生成后 apply |
| 6 | 历史会话批量分析 | 智能体 LLM 扫描历史会话 → `aizo add` 新词条 + `aizo apply`/`touch` 确认旧词条 | 定时，后台执行 |
| 7 | 每日定时任务 | 智能体 LLM 扫描日志 → `aizo apply`/`touch` 确认词条 | 定时，后台执行 |

**技能文件中的关键规则：**
- 分数极低（≤ 1.5）的词条优先级永远最高
- `recall` 返回空结果意味着无数据，而非中性偏好
- 永远不要向用户提及 aizo——它在后台静默运行
- 使用场景召回做即时上下文；不要把每一条偏好都塞进 CLAUDE.md 或其它系统设定文件

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
