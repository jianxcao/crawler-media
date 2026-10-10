# 全项目 Review 的离线复现附件

对应报告：`../2026-10-10-full-project-review.md`。基线 `277de10270f3b2ce7ccb5f30075073fdbfa614b2`；日期 2026-10-10。

这里保存 9 个复现源码文件，共 25 个测试。它们只使用 fake、临时数据库和临时文件，默认 workspace 测试不会执行 docs 下的代码。不要将这些用例当作已完成修复的回归测试：除 `review_access_tmp` 的 9 个测试断言“现有缺陷存在”而通过外，其余 16 个测试断言正确行为，在该基线上都应失败（exit 101）。Obscura 的发现另由静态生产调用链确认。

在仓库根目录执行下面的恢复代码。它先检查全部目标是否存在，避免覆盖当前工作区文件；需要仓库现有测试支持文件及 fixtures。恢复后运行指定的测试，再清理这些临时文件。

```sh
python3 - <<'PY'
from pathlib import Path
import shutil
repo = Path.cwd()
source = repo / 'docs/audits/2026-10-10-full-project-review-repro'
files = sorted(source.glob('crates/*/tests/*.rs'))
if len(files) != 9:
    raise RuntimeError('Run from the repository root; expected 9 reproduction files')
for file in files:
    target = repo / file.relative_to(source)
    if target.exists():
        raise RuntimeError(f'Would overwrite {target}; nothing copied')
for file in files:
    shutil.copy2(file, repo / file.relative_to(source))
PY
cargo test -p api --test review_pipeline_tmp -- --nocapture
cargo test -p api --test review_root_tmp -- --nocapture
cargo test -p marker --test review_root_tmp_models -- --nocapture
cargo test -p api --test review_access_tmp -- --nocapture
cargo test -p api --test review_integrations_tmp_incomplete_downloader -- --nocapture
cargo test -p downloader --test review_integrations_tmp_path_map -- --nocapture
cargo test -p media --test review_integrations_tmp_cache -- --nocapture
cargo test -p hooks --test review_integrations_tmp_login -- --nocapture
cargo test -p indexer --test review_integrations_tmp_browser -- --nocapture
```

这些命令应逐条执行并检查结果；不要因为第一个命令返回 101 而跳过后续验证。可为 Cargo 指定独立 `CARGO_TARGET_DIR`，不会改变测试行为。

验证完毕，仅当源码仍与保存的附件一致时清理：

```sh
python3 - <<'PY'
from pathlib import Path
repo = Path.cwd()
source = repo / 'docs/audits/2026-10-10-full-project-review-repro'
files = sorted(source.glob('crates/*/tests/*.rs'))
if len(files) != 9:
    raise RuntimeError('Run from the repository root; expected 9 reproduction files')
for file in files:
    target = repo / file.relative_to(source)
    if target.exists() and target.read_bytes() != file.read_bytes():
        raise RuntimeError(f'{target} changed; preserve it and inspect manually')
for file in files:
    target = repo / file.relative_to(source)
    if target.exists():
        target.unlink()
PY
```

修复时应将有价值的正确行为断言调整为正式回归测试，并按功能拆分到适当 crate；ACCESS 的用例要反转相应缺陷断言，不能直接以它们通过来判定修复完成。
