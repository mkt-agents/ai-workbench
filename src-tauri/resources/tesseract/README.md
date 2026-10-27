# Tesseract OCR 引擎（打包槽位）

天玑的「识别文字」（图片 OCR，方案 B）调用外部 **Tesseract**。随安装包分发时，把它放在这个目录，布局如下：

```
resources/tesseract/
  tesseract.exe          # Windows 可执行文件（必需）
  tessdata/
    eng.traineddata      # 英文语言包
    chi_sim.traineddata  # 简体中文语言包
    ...                  # 其它需要的语言
```

后端 `agent_ocr.rs` 的解析顺序：

1. 环境变量 `AI_WORKBENCH_TESSERACT`（指向某个 `tesseract(.exe)`）——开发机最省事；
2. 本目录 `resources/tesseract/tesseract(.exe)`——随安装包分发用；带 `tessdata/` 时会设 `TESSDATA_PREFIX` 指过来；
3. PATH 上的 `tesseract`——本机已自行安装时。

三者都没有，OCR 会**明确报「未安装 OCR 引擎」**，不会静默返回空串。

## 为什么仓库里没有二进制

Tesseract 二进制 + 中文语言包有几十 MB，不适合进 git。发布构建前由打包流程把它们放进本目录即可；`tauri.conf.json` 的 `bundle.resources` 已声明打包这个目录。

## 许可

Tesseract 是 Apache-2.0，可随商用软件分发；语言包（traineddata）同为 Apache-2.0。分发时在第三方许可清单里列明即可。

> 本目录除本说明外默认为空，属于**有意的打包占位**，不是遗漏。
