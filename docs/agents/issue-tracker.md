# Issue 跟踪：GitHub

项目初期以用户当前请求作为工作单元，不要求为每项工作创建 GitHub issue。只有用户明确要求用 GitHub 跟踪工作时，才创建、认领、评论、关闭或修改 issue。所有获准的 issue 操作用 `gh` CLI。

下列操作和导航流程仅供用户明确要求 GitHub issue 跟踪时使用，不构成日常研发流程。

Repo：`jianxcao/crawler-media`（私有）。从 `git remote -v` 推断——在 clone 内运行 `gh` 会自动完成。

## 约定

- **创建 issue**：仅在用户明确要求时运行 `gh issue create --title "..." --body "..."`。多行 body 用 heredoc。
- **读取 issue**：`gh issue view <number> --comments`，用 `jq` 过滤评论并同时取标签。
- **列出 issues**：`gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'`，配合适当的 `--label` 与 `--state` 过滤。
- **评论 issue**：`gh issue comment <number> --body "..."`
- **打/去标签**：`gh issue edit <number> --add-label "..."` / `--remove-label "..."`
- **关闭**：`gh issue close <number> --comment "..."`

## 把 PR 作为 triage 面

**PR 作为请求面：否。**

## 当 skill 说“发布到 issue tracker”

仅当用户明确要求使用 GitHub issue 跟踪时，才创建 GitHub issue。

## 当 skill 说“抓取相关 ticket”

仅在用户明确要求 issue 跟踪并给出相关 issue 时，运行 `gh issue view <number> --comments`。

## 导航操作

由 `/wayfinder` 使用。**map** 是单个 issue，**child** issues 作为 tickets。

- **Map**：一个打了 `wayfinder:map` 标签的 issue，body 承载 Notes / Decisions-so-far / Fog。`gh issue create --label wayfinder:map`。
- **Child ticket**：以 GitHub sub-issue 形式链接到 map 的 issue（`gh api` 打 sub-issues 端点）。sub-issues 不可用时，把 child 加进 map body 的 task list，并在 child body 顶部写 `Part of #<map>`。标签：`wayfinder:<type>`（`research`/`prototype`/`grilling`/`task`）。认领后，ticket 分配给主导开发的 dev。
- **Blocking**：GitHub **原生 issue 依赖**——规范、UI 可见的表示。用 `gh api --method POST repos/<owner>/<repo>/issues/<child>/dependencies/blocked_by -F issue_id=<blocker-db-id>` 加边，`<blocker-db-id>` 是 blocker 的**数据库 id**（`gh api repos/<owner>/<repo>/issues/<n> --jq .id`，*不是* `#number` 或 `node_id`）。GitHub 报告 `issue_dependencies_summary.blocked_by`（仅 open blocker——live gate）。依赖不可用时，回退到 child body 顶部的 `Blocked by: #<n>, #<n>` 行。一个 ticket 当且仅当所有 blocker 关闭才解除阻塞。
- **Frontier 查询**：列出 map 的 open children（`gh issue list --state open`，限定到 map 的 sub-issues / task list），剔除任何有 open blocker（`issue_dependencies_summary.blocked_by > 0`，或 `Blocked by` 行中的 open issue）或被 assignee 的；按 map 顺序第一个胜出。
- **认领**：仅在用户明确要求认领 issue 时运行 `gh issue edit <n> --add-assignee @me`。
- **解决**：仅在用户明确要求完成并关闭该 issue 时，运行 `gh issue comment <n> --body "<answer>"`，然后 `gh issue close <n>`；如有关联 map，再把上下文指针（gist + 链接）追加到 map 的 Decisions-so-far。
