# Billing

## Tiers

| Tier | LLM access | Vertical caps |
|------|------------|---------------|
| Free | Economic + Local only | Some vertical features locked |
| Pro | Smart tier via Terra proxy | Trader cron, knowledge graph |
| Max | Higher quotas + priority | All bundled verticals |

## Provider proxy

Managed users route chat through Terra Cloud `/v1/chat/completions`. BYOK (Settings → Advanced) bypasses quota for that provider.

## Quota

Input/output tokens deducted monthly per `hermes-billing::QuotaEngine`. Soft-stop at 100%; UI shows `QuotaWarning` below 10%.
