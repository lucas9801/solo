# README 演示视频的合成脚本

README 最上面的演示视频是"真截图 + 代码合成"：按分镜截真实界面，再由 `render.py` 逐帧合成，加字幕、镜头推拉、高亮框和鼠标点击。截图更新后重新跑一遍就能出新视频。

| 文件 | 作用 |
|---|---|
| [`docs/media/video/scenes.json`](../../docs/media/video/scenes.json) | 分镜：每个场景用哪张截图、多长、字幕、镜头、高亮、鼠标 |
| `docs/media/video/shots/` | 12 张截图（v01…v12，见下） |
| `render.py` | 合成脚本 |
| `make_placeholders.py` | 生成带坐标网格的占位截图，没有真截图时用来试分镜 |

只需要 Python 3.9+ 和 Pillow（`python -m pip install Pillow`）；编成 MP4 还要 ffmpeg。

## 1. 截图

在**干净的环境**里截（不要用公司电脑和公司项目；有企业水印的屏幕截出来的图不能用）：

- 用演示项目，路径像 `D:\demo\todo-app`；画面里不能有用户名、公司名、内网路径。
- Solo 窗口**客户区固定 1600×1000**，缩放 100%，深色主题，中文界面；12 张图同一尺寸。
- 只截 Solo 窗口（Windows 自带截图工具选"窗口"模式，或 `Alt+PrtScn`），不要截整个屏幕。
- 状态相关的画面用带红绿灯新样式（G16）的版本截。

| 文件 | 画面 |
|---|---|
| `v01-overview.png` | 演示项目打开，终端占满窗口，标题栏 3 个工作区标签 |
| `v02-agent-working.png` | claude 在工作，终端标签显示工作中和上下文百分比 |
| `v03-other-waiting.png` | 切到另一个工作区标签，那里的 agent 在等你回复（标签和状态点可见） |
| `v04-replied.png` | 同一画面，已经回复，agent 回到工作中 |
| `v05-path-in-output.png` | agent 输出里有一个文件路径（比如 `src/todo.ts`） |
| `v06-editor-open.png` | 点了路径之后，编辑器从右边打开这个文件 |
| `v07-mermaid-output.png` | agent 输出里有一段 mermaid 代码 |
| `v08-mermaid-rendered.png` | 点了之后，渲染好的图显示出来 |
| `v09-turn-done.png` | 这一轮做完，标签上出现"N 个文件已修改"和完成待看的状态 |
| `v10-review.png` | 审阅打开，能看到 2–3 个文件的改动和错误增减 |
| `v11-hunk-rejected.png` | 其中一块已撤回，一条意见已写好 |
| `v12-reopened.png` | Solo 重新打开后，agent 终端还在，仍在工作 |

## 2. 改分镜

`scenes.json` 里的坐标都是**截图上的像素**（1600×1000 的图），矩形写成 `[x, y, 宽, 高]`。现在的坐标是按占位图估的，拿到真截图后要逐个对一遍：先跑 `--preview` 看总览图，再在画图工具里读出真实位置改掉。

| 字段 | 含义 |
|---|---|
| `size`、`fps` | 输出尺寸（1280×800）和帧率（30） |
| `card` | 脚本生成的文字卡片：`title`、`subtitle`，深色背景 |
| `shot` | 单张截图 |
| `shots` | 按时间切换的几张：`[{"file": …, "until": 秒}, …, {"file": …}]`，`until` 是切到下一张的时间，前后交叉淡化 0.4 秒；其中一项也可以是 `{"card": {…}, "until": …}` |
| `duration` | 场景时长（秒）；场景之间交叉淡化 0.4 秒，不另占时间 |
| `caption` | 底部字幕；写成 `{"zh": …, "en": …}` 就能出两种语言 |
| `camera` | 镜头关键帧 `[{"t": 秒, "rect": [x, y, 宽, 高]}, …]`，中间缓入缓出；取景框会补成 16:10 并限制在截图内。不写就是整张图 |
| `highlight` | 高亮框 `[{"from": 秒, "to": 秒, "rect": [x, y, 宽, 高]}]`：3px 圆角描边 #59C2FF，框外压暗 35%，前后各淡入淡出 0.3 秒 |
| `cursor` | 鼠标关键帧 `[{"t": 秒, "at": [x, y], "click": true}, …]`，在第一个关键帧时出现，沿关键帧缓动；`click` 在那一点放 0.5 秒的扩散圆环 |

时间都是场景内的时间（从 0 开始）。

## 3. 出视频

在仓库根目录：

```powershell
python script/demo-video/render.py --lang zh --preview      # 只出每个场景中间帧的总览图：target/demo/demo-zh-preview.png
python script/demo-video/render.py --lang zh                # 出 target/demo/demo-zh.mp4 和封面 demo-zh-poster.png（要 ffmpeg）
python script/demo-video/render.py --lang en                # 英文字幕版
```

- **MP4**：1280×800、30 帧、H.264、yuv420p、`+faststart`、无音轨。从 CRF 20 开始，超过 10 MB 就加大 CRF 重编。中间帧写在临时目录，编完删除。ffmpeg 从 `--ffmpeg <路径>`、`PATH`、或者 `imageio-ffmpeg` 包里找；都没有时脚本报错退出，并提示安装方法（`winget install Gyan.FFmpeg`，或 `pip install imageio-ffmpeg`）。
- **没有 ffmpeg**：`--frames-only` 把逐帧 PNG 留在 `target/demo/frames-zh/`（57 秒约 1710 帧、170 MB 左右），并打印编码命令，在有 ffmpeg 的机器上执行：

  ```powershell
  ffmpeg -y -framerate 30 -i target/demo/frames-zh/%05d.png -c:v libx264 -preset slow -crf 23 -pix_fmt yuv420p -movflags +faststart -an target/demo/demo-zh.mp4
  ```

- **动图预览**：`--anim webp`（或 `gif`、`png` 即 APNG）只用 Pillow 另出一个缩小的动图，默认宽 640、10 帧（`--anim-width`、`--anim-fps`），方便没有 ffmpeg 时看效果。
- **样片**：`--only 1,5,10` 只渲染这几个场景。
- **占位图**：`python script/demo-video/make_placeholders.py <目录>` 生成 12 张带坐标网格的占位截图，再用 `--shots <目录>` 让脚本用它们。
- 同样的输入每次得到同样的帧。

字体用 Noto Sans SC（SIL Open Font License 1.1），按这个顺序找：`--font <文件>`、环境变量 `SOLO_DEMO_FONT`、本目录的 `fonts/`（`NotoSansSC-VF.ttf`）、`C:\Windows\Fonts\NotoSansSC-VF.ttf`。字体文件没有放进仓库（17 MB）；要放的话从 [notofonts/noto-cjk](https://github.com/notofonts/noto-cjk) 下载可变字体，连同它的 `LICENSE`（OFL）一起放进 `fonts/`。

## 4. 放进 README

在 GitHub 的 issue 或 PR 编辑框里拖入 `demo-zh.mp4`，会得到一个 `https://github.com/user-attachments/assets/…` 链接。把这个链接单独放在 README 的演示视频那一行，替换掉"演示视频（待补）"那段，GitHub 会显示成播放器。封面图 `demo-zh-poster.png` 可以放进 `docs/media/`，在不能播放视频的地方用。
