# Agent conventions for this repository

## 构建约束 (Build constraints)

重型 Rust build/test 必须通过 `heavy-cargo`（不要直接并发多个 `cargo test`/`cargo build`）。装了全局版就用全局版；`~/.local/bin/heavy-cargo` 不存在时用仓库内入口，禁止回退裸 cargo：

```bash
heavy_cargo=./scripts/heavy-cargo
[ ! -x ~/.local/bin/heavy-cargo ] || heavy_cargo=~/.local/bin/heavy-cargo
"$heavy_cargo" test -p phone-gateway -p server
"$heavy_cargo" test -p phone-gateway -p server --durations
"$heavy_cargo" build --release --bin iphone-use
```

- 仓库入口默认编译并发 = `max(1, CPU 核数 // 4)`；`CARGO_BUILD_JOBS` 或 `-j N` 可调，硬上限为 `max(1, CPU 核数 // 2)`，超限提示并截断。
- `test` 自动使用同值 `--test-threads`；调用方已指定时保留。`--durations` 在稳定版 Rust 上逐个计时、按上述线程数并发，输出最慢 10 个用例。
- 用 `$TMPDIR` 下同用户共享的 flock 锁保证一次只有一个重型 Cargo；等待时提示。macOS 无 flock 命令时用 Python 3 的 `fcntl.flock`，需安装 Python 3。
- 入口尝试将自身优先级降为 nice 10（失败忽略）；安装了 sccache 时设置 `RUSTC_WRAPPER`，否则跳过。
- 长延迟回归可用 `IPHONE_USE_SLOW_TESTS=1` 恢复原始模拟时长；默认用测试请求参数缩短，生产默认值不变。
- 默认跳过的真机 / Xcode 测试保持原开关不变；显式重用例使用各测试说明的环境变量 / `-- --ignored`，绘图样例为 `WIREFRAME_SAMPLE=/path/out.png` 配合 `-- --ignored`。
- 裸 `cargo` 仅用于轻量操作（`cargo fmt`、`cargo clippy` 等）。多个 worktree 保持独立 `target/`，不设全局 `CARGO_TARGET_DIR`；跨 worktree 复用靠 sccache。

## Shared worktree discipline

Several agent sessions commit from this one checkout. Before every commit:
`git status` and `git diff --cached --stat`, stage **by file** (`git add <path>`), and never use `git add -A` / `git commit -a`. A file you did not edit that shows as modified belongs to another session — leave it. On 2026-09-05 a `git add` of `scripts/setup-runner.sh` swept another session's half-finished multi-instance rewrite into v0.6.1 and broke the release gate. Attribution: read the `Claude-Session:` trailer, not the author name (all sessions share it).
