interface TaskProgressDashboardProps {
  progress?: number
  costCents?: number
}

export function TaskProgressDashboard({ progress = 0, costCents = 0 }: TaskProgressDashboardProps) {
  return (
    <div className="terra-task-progress-dashboard">
      <progress max={100} value={progress} />
      <span>{costCents}¢</span>
    </div>
  )
}

export default TaskProgressDashboard
