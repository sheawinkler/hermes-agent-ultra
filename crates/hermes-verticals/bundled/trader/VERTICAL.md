---
[meta]
id = "trader"
display_name_key = "vertical.trader.name"
description_key = "vertical.trader.description"
icon = "chart"
category = "finance"
order = 100
task_category = "Financial"

[provider]
default_tier = "smart"

[provider.tier_overrides]
smart = "tongyi-qwen-max"
economic = "tongyi-qwen-turbo"
local = "ollama-qwen3-32b"

[datasources]
default = "akshare"
default_mode = "cloud"
allowed = ["akshare", "user_custom"]
data_delay_disclosure_key = "vertical.trader.disclaimer.delay_15min"

[privacy.data_egress]
providers_smart = ["aliyun-tongyi"]
providers_economic = ["aliyun-tongyi"]
providers_local = []
disclosure_key = "vertical.trader.privacy.disclosure"
require_explicit_consent = true

[persona]
strategy = "auto_blend"

[[persona.blocks]]
kind = "instruction"
follow_user_locale = false
variants = { en = "instruction.en.md", "zh-CN" = "instruction.zh-CN.md" }

[[persona.blocks]]
kind = "terminology"
follow_user_locale = true
variants = { en = "glossary.en.md", "zh-CN" = "glossary.zh-CN.md" }

[[persona.blocks]]
kind = "output_directive"
follow_user_locale = true
---

# Trader Vertical

A-share market analysis assistant.
