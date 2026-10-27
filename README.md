# batchbench

GitHub 仓库：https://github.com/stevensu1977/httptools

基于 **httpress Rust 库**的 HTTP 批量请求压测工具，默认使用 POST，可配置请求方法。目标场景是单台 m5.2xlarge
发起约 **10,000 个 HTTP POST/秒**，每个请求默认随机包含 **1–10 条事件**。
目标速率是发送计划，实际成功吞吐以结果报告为准。

选择 httpress 的原因是它提供 `request_fn`，可在每次请求时生成数据，并复用并发执行、
连接池和统计能力。Drill 更适合 YAML 多步骤流程；Plumbrs 更偏向静态请求体的吞吐测试。
这里使用 httpress 0.6.0 的本地副本，补充重定向、完整响应超时和固定速率调度，
具体改动见 [PATCHES.md](vendor/httpress/PATCHES.md)，上游 MIT 许可证保留。

## 编译与配置

需要 Rust 2024 edition 工具链；已使用 Rust 1.93.1 验证。Ubuntu 编译依赖：

```bash
sudo apt-get install -y build-essential pkg-config libssl-dev
git clone https://github.com/stevensu1977/httptools.git
cd httptools
cargo build --release --locked
```

在项目目录运行。程序自动加载 `.env` 中的 `ACCESS_TOKEN`、`ENDPOINT` 和 `METHOD`：

```dotenv
ACCESS_TOKEN=这里填写Basic认证所用的Base64凭据
ENDPOINT=https://zt0b6mmsrc.execute-api.us-east-2.amazonaws.com/test/v1/batch
METHOD=POST
```

配置优先级：**命令行参数 > 已导出的同名环境变量 > `.env` > 内置默认值**。
变量名使用大写；方法值不区分大小写，例如 `POST`、`post`、`Patch`。

| 配置 | 环境变量 | 命令行参数 | 默认值 |
| --- | --- | --- | --- |
| 请求地址 | `ENDPOINT` | `--endpoint`，兼容别名 `--url` | 原始 API Gateway `/test/v1/batch` 地址 |
| HTTP 方法 | `METHOD` | `--method` | `POST` |
| Basic 凭据 | `ACCESS_TOKEN` | `--auth-env` 可改为读取其他变量名 | 必须配置，`--dry-run` 除外 |

程序生成 `Authorization: Basic <ACCESS_TOKEN>`；也接受带 `Basic ` 前缀的值。
它不会将原始用户名/密码自动编码，也不会将值作为 Bearer token 使用。
`.env` 已加入 `.gitignore`，配置样例见 [.env.example](.env.example)。

可以直接在命令行设置环境变量，覆盖 `.env` 中的地址和方法：

```bash
ENDPOINT='http://127.0.0.1:3000/batch' METHOD=PUT \
  ./target/release/batchbench --requests 100 --batch-size 10
```

也可以用参数覆盖环境变量：

```bash
./target/release/batchbench \
  --endpoint 'http://127.0.0.1:3000/batch' \
  --method PATCH \
  --events 10000 --batch-size 1-10
```

`--endpoint` 与 `--url` 是同一个参数的两种名称。支持的方法为
`GET`、`POST`、`PUT`、`PATCH`、`DELETE`、`HEAD`、`OPTIONS`。
修改方法只改变 HTTP 方法，请求体仍然是模板生成的 batch JSON，不会转换成查询参数。
生效的地址或方法无效时，程序在发送请求前报错；不会静默回退到默认值。

## 使用

先预览 JSON，不发送请求，也不要求认证：

```bash
./target/release/batchbench --dry-run --batch-size 1
```

以下命令使用配置的地址和方法发送一个请求。未配置时，使用原始 API Gateway 地址和 POST：

```bash
./target/release/batchbench --batch-size 1
```

按目标配置运行 60 秒：

```bash
./target/release/batchbench \
  --duration 60s \
  --concurrency 1024 \
  --rate 10000 \
  --batch-size 1-10 \
  --template templates/batch.json \
  --json
```

- `--rate` 始终表示每秒 HTTP 请求数，与事件数无关；省略时不限制速率。
- `--batch-size 1-10` 对每个请求均匀随机选择 1–10；`--batch-size 10` 固定为 10。
  随机 1–10 时平均约 5.5 条事件，10,000 HTTP RPS 对应平均约 55,000 条事件/秒。
- `--concurrency` 是并发 worker 数，每个 worker 最多有一个在途请求。
- `--requests 100` / `-n 100` 指定 HTTP 请求总数。
- `--events 100000` 指定尝试发送的事件记录总数，最后一个 batch 自动缩小，避免超发。
- `--requests`、`--events`、`--duration` 三者互斥；均未指定则只发 1 次请求。
- `--timeout 10s` 包含连接、重定向、读取完整响应的时间。
- `--endpoint http://127.0.0.1:3000/batch`（或 `--url`）覆盖 `ENDPOINT`。
- `--method PUT` 覆盖 `METHOD`。

默认复用 HTTP/1.1 Keep-Alive 连接、校验 HTTPS 证书，不自动重试。
自动跟随最多 50 次重定向，行为对应 curl `--location`：
301/302/303 的 POST 改为 GET；307/308 保留 POST 和原始 body；
跨 origin 不转发认证。压测时应使用最终 URL，避免重定向增加实际网络请求数。

## 控制发送总量

精确发送 **100,000 条事件记录**，每批随机 1–10 条，完成后自动停止：

```bash
./target/release/batchbench \
  --events 100000 \
  --batch-size 1-10 \
  --concurrency 1024 \
  --rate 10000 \
  --output results/events-100000.json
```

`--rate 10000` 仍然控制 HTTP 请求/秒。事件总量模式中，HTTP 请求总数取决于实际 batch
大小；并发 worker 共享剩余配额，不会因为并发而超发。剩余记录数不足时，最后分配的 batch
允许小于配置的最小值。例如 `--events 103 --batch-size 10` 会发送 11 个 HTTP 请求，
包含 10 个 10 条的 batch 和 1 个 3 条的 batch。

如果要控制 HTTP 请求总数，使用：

```bash
./target/release/batchbench --requests 10000 --batch-size 10 --concurrency 512
```

这会尝试发送 10,000 个 HTTP 请求，合计 100,000 条事件记录。

`--events` 按**尝试发送**计数；429、500、连接错误、超时等失败也消耗配额，
不会额外重试或补发来凑成功记录数。它不保证服务端实际收到或成功入库的数量。
报告中的 `target_events` 是目标记录数，`events_in_attempted_http_requests` 是尝试发送数，
`events_in_successful_http_responses` 是返回 2xx 的请求所携带的记录数，
`event_limit_reached` 表示是否已尝试发送全部目标记录。Ctrl+C 会停止新请求并等待在途请求，
可能使尝试发送数少于目标；这种情况退出码为 `1`。

## 实时事件进度

在终端运行时，默认每 250 ms 更新一次事件进度。例如：

```text
Events [running] completed=40000/100000 (40.0%) remaining=60000
in_flight=1000 unsent=59000 successful=39990 failed=10
```

- `completed`：已完成请求中的事件数，包括成功和失败。
- `remaining`：目标事件数减去已完成数，包括尚未发出和正在等待响应的记录。
- `in_flight`：已开始发送、尚未完成请求的事件数。
- `unsent`：尚未开始发送的事件数。
- `successful` / `failed`：成功（HTTP 2xx）和失败请求中携带的事件数。

例如全部请求已发出但还在等待响应时，`unsent=0`，`remaining` 仍大于 0。
进度达到 100% 表示所有事件对应的请求已结束，业务是否成功仍需查看 `failed` 和服务端结果。
完成或 Ctrl+C 后会保留最终进度；提前停止时显示 `[stopped]` 和剩余数量。

使用 `--events` 才能提前知道事件总数；只指定 `--requests` 或 `--duration` 时，
显示已尝试、已完成、在途、成功和失败数量，不显示未知的总数或剩余百分比。

进度写入 **stderr**，JSON 结果写入 stdout，因此可以同时显示进度并保存 JSON。
stderr 被重定向时默认关闭进度；使用 `--progress` 可强制输出逐行进度日志：

```bash
./target/release/batchbench \
  --events 100000 --batch-size 1-10 --concurrency 1024 --rate 10000 \
  --progress --json > results.json
```

使用 `--no-progress` 可以关闭实时显示，不能与 `--progress` 同时使用。
最终 JSON/CSV 报告也包含 `completed_events`、`remaining_events`、
`in_flight_events`、`failed_events`，其中未指定事件目标时 `remaining_events` 为 `null`。

## 模板

**模板文件需要通过 `--template` 显式指定，不会自动加载。**
省略该参数时使用程序内置模板，其中 `event` 是 `curl_ok`；
仅修改磁盘上的 `templates/batch.json` 不会影响这种运行方式。

先预览实际请求，再带上相同的模板路径运行：

```bash
./target/release/batchbench --template templates/batch.json --dry-run --batch-size 1

./target/release/batchbench \
  --template templates/batch.json \
  --events 100000 --batch-size 1-10 --concurrency 1024 --rate 10000
```

JSON 文件在**每次启动时**读取，修改后重新启动工具即可生效，无需重新编译 Rust。
运行中的进程使用启动时已经编译的模板，不会热加载后续文件修改。
相对路径以启动命令所在目录为准；在其他目录运行时，可指定模板的绝对路径。
命令行 `--event`、`--message-id`、`--anonymous-id` 优先于文件里的对应字段。

[templates/batch.json](templates/batch.json)：

```json
{
  "event": "httptools_ok",
  "messageId": "m-{{run_id}}-{{seq}}",
  "anonymousId": "a-{{seq}}"
}
```

支持命令行覆盖文件中的字段：

```bash
./target/release/batchbench --dry-run --batch-size 2 \
  --event 'purchase_{{request_seq}}' \
  --message-id '{{uuid}}' \
  --anonymous-id 'user_{{worker_id}}'
```

| 占位符 | 含义 |
| --- | --- |
| `{{seq}}` | 本次运行的全局事件序号，从 1 开始，跨 worker、跨 batch 唯一 |
| `{{request_seq}}` | 全局请求序号，从 1 开始；同一 batch 相同 |
| `{{batch_index}}` | 当前事件在 batch 内的序号，从 1 开始 |
| `{{worker_id}}` | worker 编号，从 0 开始；可模拟固定用户 |
| `{{uuid}}` | 每条事件生成 UUID v4，同一事件的三个字段共享该值 |
| `{{run_id}}` | 本次运行固定 UUID；默认 messageId 使用它避免跨运行重复 |
| `{{timestamp_ms}}` | 当前请求生成时的机器 Unix 毫秒时间 |

这是一套预编译的占位符模板，支持以上变量和固定文本，不执行脚本或表达式。
未知变量在启动时直接报错；JSON 使用序列化器转义，支持中文、引号和换行。
并发时序号表示生成顺序，不保证服务端收到的顺序。

使用上述模板文件时，请求结构如下，`type` 固定为 `track`，`sentAt` 每次发送前读取机器时钟，
以 UTC RFC 3339 格式输出。默认时间精度为毫秒，因此不同请求可以有相同时间戳：

```json
{
  "batch": [
    {
      "type": "track",
      "event": "httptools_ok",
      "messageId": "m-<run_id>-1",
      "anonymousId": "a-1"
    }
  ],
  "sentAt": "2026-10-26T09:00:00.123Z"
}
```

## m5.2xlarge 上达到 10,000 HTTP RPS

m5.2xlarge 为 8 vCPU / 32 GiB。先用 release 构建，压测机尽量与目标服务同区域；
时钟同步影响 `sentAt` 的准确性。建议提高当前 shell 的文件描述符上限：

```bash
ulimit -n 65535
```

并发需求大致为 **RPS × 平均响应时间（秒）**：

| 平均响应时间 | 10,000 RPS 所需在途请求数 |
| --- | --- |
| 20 ms | 200 |
| 50 ms | 500 |
| 100 ms | 1,000 |
| 200 ms | 2,000 |

从 `--concurrency 512` 或 `1024` 开始，根据实际延迟调整。先用低速率验证请求，
再依次测试 1,000、5,000、10,000 RPS。若 CPU 有余量但吞吐偏低，可增加并发；
若出现大量 429，应检查 API Gateway 的账号/区域、stage/route、usage plan 等适用限额，
以及后端处理能力。batch=10 时服务端每秒需处理 100,000 条事件。

`--rate 10000` 是目标发送频率，操作系统调度、TLS 建连、尾部请求收尾都可能使报告值
略低于 10,000。严格验收“成功吞吐 ≥ 10,000”时，需要在服务端允许的情况下预留发送速率
和并发余量，并通过持续测试确认；仅设置参数不能保证达标。

## 生成统计报告

默认终端输出 p50、p90、p95、p99、平均/最小/最大延迟、RPS、请求成功/失败数、
完整状态码分布及非 200 响应统计。
`--json` 将终端输出改为 JSON，`latency_ms` 下的所有延迟数值单位均为毫秒。

使用 `--output` 保存报告，扩展名选择 JSON 或 CSV；可重复指定，一次压测生成两种格式：

```bash
./target/release/batchbench \
  --duration 60s --concurrency 1024 --rate 10000 \
  --batch-size 1-10 \
  --output results/run-001.json \
  --output results/run-001.csv
```

报告目录会自动创建；文件名必须是新文件，已有报告不会被覆盖。
输出路径在发送请求前检查。存在 429、500 或超时等失败时仍会保存报告，
同时保持相应的非零退出码。CSV 每次运行输出一行，延迟列名为
`latency_p50_ms`、`latency_p99_ms` 等，可以在 Excel 中查看和比较。
`--output` 可与 `--json` 同用，不能与 `--dry-run` 同用。

| 指标 | 含义 |
| --- | --- |
| p50 | 中位延迟，至少 50% 的已完成请求耗时不超过该值 |
| p90 / p95 / p99 | 至少 90% / 95% / 99% 的已完成请求耗时不超过该值 |
| mean / min / max | 平均、最小、最大延迟 |
| `actual_http_rps` | 每秒完成的请求数，包含失败 |
| `successful_http_rps` | 每秒成功请求数，验收 10,000 TPS 时使用此值 |
| `success_percent` | HTTP 2xx 占已完成请求的百分比 |
| `transport_errors` | 连接、超时、响应体读取等传输失败数 |
| `http_responses` | 已完整读取的最终 HTTP 响应数，不包含传输错误 |
| `non_200_responses` | 状态码不等于 200 的响应总数 |
| `non_200_percent` | 非 200 响应占 `http_responses` 的百分比，无响应时为 0 |
| `non_200_status_codes` | 非 200 状态码及各自的响应数量 |

非 200 统计默认开启，无需增加参数，终端和 JSON/CSV 都会包含。例如以下报告片段表示
100 个 HTTP 响应中有 15 个非 200 响应：

```json
{
  "http_responses": 100,
  "non_200_responses": 15,
  "non_200_percent": 15.0,
  "non_200_status_codes": {
    "201": 2,
    "429": 10,
    "500": 3
  }
}
```

这里按 HTTP 响应数统计，不是事件记录数。201、202、204 等也属于“非 200”，
但它们仍按现有规则计为成功的 2xx 响应；非 200 不等于 `failed_http_requests`。
没有完整 HTTP 响应的连接错误、超时等计入 `transport_errors`，不计入非 200 数量或占比。
跟随重定向时统计最终响应状态码，中间的 301/302/307 等跳转不会另外计数。
`status_codes` 继续保留全部最终状态码（含 200）的分布。

分位数使用 nearest-rank 算法：对所有请求延迟排序，取第 `ceil(N × p / 100)` 个样本。
例如 p99=80 ms 表示至少 99% 的样本延迟不超过 80 ms。
小样本的 p99 往往就是最大值，判断尾延迟应使用足够多的请求。

## 统计口径

`--json` 输出请求数量、目标/实际 RPS、成功 RPS、响应状态码分布、
传输错误数、响应字节数及 p50/p90/p95/p99 延迟，事件数量按实际随机 batch 累计。

- `actual_http_rps` 包括成功与失败；验收使用 `successful_http_rps`。
- `events_in_successful_http_responses` 是返回 2xx 的请求中携带的事件数。
  工具不解析业务响应，也不验证下游落库，不能将该值当作最终业务入库量。
- 延迟包含模板生成、完整响应和重定向，不包含请求发送前的限速等待。
  样本包含 HTTP 失败和传输失败，并非只统计成功请求；
  `latency_sample_count` 为样本数量，`latency_scope` 为 `all_completed_requests`。
  统计只覆盖实际发出的请求，不包含因并发不足而未发出的计划请求。
- `--duration` 限制新请求发送窗口；已在途请求会收尾，报告耗时和 RPS 包含这段时间。
- 退出码：`0` 为已完成请求全部返回 2xx 且事件目标（若指定）已完成；
  `1` 为存在 HTTP/传输失败或事件目标未完成；
  `2` 为配置、认证或执行错误。退出码不代表达到目标速率。
- 固定速率使用错开的 worker 定时器；调度落后时跳过错过的 tick，不补发突发流量。

httpress 保留每次请求的延迟样本，内存随请求总量增长。适合有限时长的压测；
高吞吐下不建议一次连续运行数天。

## 本地验证

配置测试验证默认值、`.env`、shell 环境变量、命令行覆盖、`--url` 别名、方法大小写和非法值；
它使用 `--dry-run`，无需网络或认证：

```bash
cargo test --test config --locked
```

完整测试还通过本地 HTTP 服务验证实际发送的方法、地址、请求体以及统计行为：

```bash
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --examples --locked
```

终端一启动模拟服务。它读取请求体并立即响应，不执行真实业务：

```bash
./target/release/examples/mock_server
```

终端二运行压测，用测试凭据覆盖 `.env`：

```bash
ACCESS_TOKEN=dXNlcjpwYXNz ./target/release/batchbench \
  --endpoint http://127.0.0.1:3000/batch --method POST \
  --duration 10s --rate 10000 --concurrency 512 \
  --batch-size 1-10 --json
```

模拟服务也支持 `--delay-ms 50`，用于观察延迟与并发的关系。
本地 HTTP 回环结果只验证压测端的基本吞吐能力；
真实 HTTPS、网络和 API Gateway 后端需要在目标 m5.2xlarge 上单独验证。

本次本地测试测得约 12,000 个成功 POST/秒，详细环境、参数和结果见
[本地吞吐验证记录](docs/local-benchmark.md)。
