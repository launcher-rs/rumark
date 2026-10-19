# 图片渲染与交互测试文档

用于回归验证图片解析、渲染与右键菜单。本文件依赖同目录下的 `assets/` 资源：

- `docs/assets/red.png`（160×100）
- `docs/assets/blue.png`（240×160）
- `docs/assets/icon.png`（64×64）

打开本文件时请确认已保存到磁盘（未保存文档没有基准目录，相对路径无法解析）。

## 1. 本地相对路径

![红色方块](./assets/red.png)

## 2. 带标题与空替代文本

![带标题的图片](./assets/blue.png "图片标题")

![](./assets/icon.png)

## 3. 路径写法

反斜杠分隔（Windows 风格）：

![反斜杠](assets\red.png)

带 `./` 前缀：

![点斜杠](./assets/blue.png)

使用 `../` 回到上级再进入：

![上级目录](../docs/assets/icon.png)

绝对路径（本机不存在时渲染为错误态，可自行改成真实路径）：

![绝对路径](C:\Windows\Web\Wallpaper\red.png)

## 4. 重复引用同一文件

![第一次](./assets/icon.png)

![第二次](./assets/icon.png)

## 5. 远程 URL

![远程 PNG](https://example.com/image.png)

![带标题的远程图片](https://example.com/image.png "远程标题")

## 6. 引用式图片

![完整引用][full-ref]

![折叠引用][]

![快捷引用]

[full-ref]: ./assets/red.png "完整引用标题"
[折叠引用]: ./assets/blue.png
[快捷引用]: ./assets/icon.png

## 7. 链接包裹的图片

[![点击跳转](./assets/icon.png)](https://example.com)

## 8. 列表与引用中的图片

- 列表项一：![列表图片](./assets/red.png)
- 列表项二：文本 + ![行内图片](./assets/icon.png) + 文本

1. 有序一：![有序图片](./assets/blue.png)
2. 有序二

> 引用中的图片：
>
> ![引用图片](./assets/red.png)

- [ ] 任务项：![任务图片](./assets/icon.png)
- [x] 已完成任务

## 9. 表格中的图片

| 列一 | 列二 | 列三 |
| --- | --- | --- |
| 文本 | ![单元格图片](./assets/icon.png) | ![另一张](./assets/red.png) |
| ![行内图片](./assets/blue.png) 与文本 | 尾巴 | 结尾 |

## 10. 内嵌 base64（data URI）

PNG（64×64 绿色图标的 base64 载荷）：

![内嵌 PNG](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAYAAACqaXHeAAAAAXNSR0IArs4c6QAAAARnQU1BAACxjwv8YQUAAAAJcEhZcwAADsMAAA7DAcdvqGQAAAHHSURBVHhe7Za9bcMwEIW9UZbIHFrAa7j2CCndCVrBjQuX3kCA4N4bMBAESsdH3tEIRBd5R+Ar8izmTh9/7MPXz3dg5oABGy4AAzZcAAZsuAAM2HABGLDhAjBgwwVgwIYLwIANF4ABGy4AAzZcAAZsuAAM2HABGJToHs+AY3ocs+dWhj5M8cFXHzr8/A+06qEi4Bxuac11WMVPo3jQKP4ebXswBBzD5aUX6wal+PUuKtvF67TvQRcg/gkW1imsliiur4qc9wyXoV0PiCpga/YeToXPS6xzxn5buaR4uqK3K9basnY9pCgChMV5Mm6p8ZzN2S6puVnxolgcLyf8+xM9COoCtCEbWJuL29cuLld8esWmxdb/QA+RtwRs27JwVovntFY8f7n8jLfuYaEuACbjeU0uNmOkL5gLyLd06x4WFAHyskovoD2Ka3PkBdi6h4giQNlW8sIyb2Zj+8nLDC8349ldexDoAuArC0e6WohWvHB+zR87LXpIMQQs5L/BLeuRUnF4meINPg/8Ntizh5yqgP+OC8CADReAARsuAAM2XAAGbLgADNhwARiw4QIwYMMFYMCGC8CADReAARsuAAM2XAAGbNAL+AX724574N84WwAAAABJRU5ErkJggg==)

内嵌 SVG：

![内嵌 SVG](data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIxMjAiIGhlaWdodD0iNjAiPjxyZWN0IHdpZHRoPSIxMjAiIGhlaWdodD0iNjAiIGZpbGw9IiM1QjhERUYiLz48dGV4dCB4PSI2MCIgeT0iMzYiIGZvbnQtc2l6ZT0iMTYiIGZpbGw9IndoaXRlIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIj5TVkc8L3RleHQ+PC9zdmc+)

## 11. HTML 图片

行内 HTML：这段文字后面跟一个 <img src="./assets/icon.png" alt="行内 HTML 图片"> 图片。

块级 HTML：

<img src="./assets/red.png" alt="块级 HTML 图片" width="120" />

## 12. 错误与边界

缺失文件：

![缺失的图片](./assets/does-not-exist.png)

不是图片扩展名：

![伪装成图片](./assets/notes.txt)

转义的图片语法（应显示为字面文本，不渲染）：

\![转义图片](./assets/red.png)

混合文本（不是独立图片，应保持为文本）：

前缀文本 ![图片](./assets/red.png) 后缀文本

## 13. 右键菜单测试清单

在图片上点右键，逐项验证：

1. 打开图片位置
2. 复制图片到
3. 复制图片
4. 缩放图片（子菜单）
5. 复制 / 粘贴为...
6. 段落、插入等通用项仍然可用
