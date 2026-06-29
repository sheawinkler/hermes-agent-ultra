interface CostDashboardProps {
  inputTokens?: number
  outputTokens?: number
  providerCalls?: number
}

export function CostDashboard({ inputTokens = 0, outputTokens = 0, providerCalls = 0 }: CostDashboardProps) {
  return (
    <div className="terra-cost-dashboard">
      <span>in: {inputTokens}</span>
      <span>out: {outputTokens}</span>
      <span>calls: {providerCalls}</span>
    </div>
  )
}

export default CostDashboard
