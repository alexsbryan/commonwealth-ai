# 02_should_search_stock_price

## What this proves

Realtime financial data can't come from training. Pin that the model
searches for stock prices using a tight ticker-based query.

## Mock corpus

`nvda-stock-quote.json` covers seven phrasings through `aliases.toml`
(nvda price / nvidia stock price / current nvda / etc).

## Known sensitivities

- If the model produces a query like "current price of NVIDIA NVDA on
  the stock market today" (too long), `expected_query_max_tokens = 6`
  fails it. Tighten the system prompt or relax the cap.
