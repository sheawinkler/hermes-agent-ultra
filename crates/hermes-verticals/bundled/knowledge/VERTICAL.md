---
[meta]
id = "knowledge"
display_name_key = "vertical.knowledge.name"
description_key = "vertical.knowledge.description"
icon = "book"
category = "productivity"
order = 200
task_category = "GeneralChat"

[provider]
default_tier = "smart"

[provider.tier_overrides]
smart = "kimi-k2"
economic = "kimi-32k"
local = "ollama-qwen3-14b"

[datasources]
default = "akshare"
default_mode = "cloud"
allowed = ["user_custom"]

[persona]
strategy = "auto_blend"

[[persona.blocks]]
kind = "instruction"
follow_user_locale = false
variants = { en = "instruction.en.md", "zh-CN" = "instruction.zh-CN.md" }

[[persona.blocks]]
kind = "output_directive"
follow_user_locale = true
---

# Knowledge Vertical

Organize URLs, images, audio, and video into structured knowledge.
