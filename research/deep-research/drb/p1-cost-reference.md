# P1 cost reference — the named proxy (frozen)

P1's reference is a NAMED proxy arm, never "cloud DR" in general. The named
proxy is OpenAI's published API rates for o3-deep-research at the typical
per-task usage mix. This file freezes the arithmetic and the citations.

## The reference arithmetic (per task)

| Component | Rate (published) | Typical per-task usage | Cost |
|---|---|---|---|
| Input tokens | $10.00 / 1M | 50K | $0.50 |
| Output tokens | $40.00 / 1M | 20K | $0.80 |
| Web search calls | $10.00 / 1K | 15 | $0.15 |
| **Total** | | | **$1.45 / task** |

The "typical per-task usage" mix (50K input / 20K output / 15 searches) is
the assumption named by the pricing analysis cited below. The rate column is
OpenAI's published o3-deep-research API pricing, confirmed by the OpenAI
Developer Community announcement ("Deep research in the API, webhooks, and
web search with o3") and mirrored by the pricing trackers.

## Citations

- Rates: OpenAI Developer Community, "Deep research in the API, webhooks,
  and web search with o3" —
  https://community.openai.com/t/deep-research-in-the-api-webhooks-and-web-search-with-o3/1299919
- Typical-mix arithmetic: TokenCost, "OpenAI Deep Research API pricing 2026:
  o3 vs o4-mini" — https://tokencost.app/blog/openai-deep-research-api-pricing
  (section "What a query actually costs"; assumption stated on the page:
  "Assumes 50K input tokens, 20K output tokens, 15 web searches"; arithmetic
  shown: input $0.50 + output $0.80 + search $0.15 = ~$1.45 per query)

## The measured side (named constants)

Local per-task cost = wall_time_seconds x 60 W x $0.15/kWh / 3600

- wall_time_seconds: from the run manifest's lock timestamps
  (acquired_unix -> released_unix)
- 60 W: the host's sustained draw for the deep-research flight (the local
  daemon on :9741; a Qwen3.6-35B-A3B-MTP-UD-Q6_K draft on the Strix Halo iGPU)
- $0.15/kWh: the operator's named residential electricity rate (fixed,
  not measured)

Raw measurements are also reported alongside the cost: wall time,
acquisition units (searches + fetches), report length in words. The cost
figure is the primary P1 statistic; the raws are descriptive.
