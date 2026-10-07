# 领域文档

工程技能在探索代码库时应如何消费本仓库的领域文档。

## 探索之前先读这些

- 仓库根的 **`CONTEXT.md`**
- **`docs/adr/`** —— 阅读与你将工作的区域相关的 ADR
- **`docs/agents/roadmap.md`** —— 当前状态与历史（grilling 时代的 `docs/spec.md` 已作为过时移除）
- **`AGENTS.md`** —— 用户请求如何工作、测试 seam、Rust 体积限制

若这些文件中有任何不存在，**静默继续**。不要标记其缺失；不要主动建议创建它们。`/domain-modeling` skill（经 `/grill-with-docs` 与 `/improve-codebase-architecture` 触达）在术语或决策实际被解析时才惰性创建它们。

## 文件结构

单上下文仓库：

```
/
├── CONTEXT.md
├── docs/adr/
├── docs/agents/roadmap.md
└── crates/
```

## 使用 glossary 的词汇

当你的输出命名领域概念（issue 标题、重构提案、测试名）时，使用 `CONTEXT.md` 中定义的术语。不要漂移到 glossary 明确回避的同义词。

如果 glossary 中还没有你需要的概念，那是一个信号——要么你在发明项目不用的语言（重新考虑），要么存在真实缺口（为 `/domain-modeling` 记一笔）。

## 标记 ADR 冲突

若你的输出与既有 ADR 冲突，明确标出，而不是静默覆盖。
