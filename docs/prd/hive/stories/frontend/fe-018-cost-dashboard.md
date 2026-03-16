# [FE-018] Cost Dashboard with Per-Agent Charts

**As a** workspace admin
**I want to** see cost breakdowns and trends across agents and models
**So that** I can manage budget, identify expensive agents, and optimize model usage

## Acceptance Criteria
- [ ] The Costs tab renders a `<CostChart>` dashboard with three chart sections: cost over time (line chart), cost by model (bar chart), and cost by agent (horizontal bar chart)
- [ ] The "cost over time" chart shows daily spending for the last 30 days by default, with selectable time ranges (7d, 30d, 90d, custom date range)
- [ ] The "cost by model" chart breaks down spending per model (e.g., claude-sonnet, claude-opus) with percentage labels
- [ ] The "cost by agent" chart ranks agents by total cost in the selected time period, with each bar segmented by model type
- [ ] A summary card at the top displays: total spend in the selected period, average daily cost, projected monthly cost (based on trailing 7-day average), and comparison to the previous period (percentage change with up/down indicator)
- [ ] Hovering over any chart data point shows a tooltip with the exact dollar amount, date, and breakdown
- [ ] Budget alerts: if a configurable monthly budget threshold is set, a warning banner appears when spending exceeds 80% of the budget, and turns red at 100%
- [ ] All cost data is fetched from the Hive server's cost API; charts update when new cost events arrive via WebSocket

## Phase
Phase 4: Advanced

## Priority
P2

## Components
- CostChart

## Notes
Cost data is tracked per-agent by room-ralph (token usage from Claude API responses). The Hive server aggregates this data. Chart rendering should use a lightweight library (e.g., Chart.js, Recharts for React, or LayerChart for Svelte). The PRD references cloud billing dashboards (AWS, GCP) as design inspiration. Currency display should be configurable (USD default).
