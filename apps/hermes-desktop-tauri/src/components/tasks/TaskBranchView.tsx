interface TaskBranchViewProps {
  rootTaskId: string
  branchIds?: string[]
}

export function TaskBranchView({ rootTaskId, branchIds = [] }: TaskBranchViewProps) {
  return (
    <div className="terra-task-branch-view" data-root={rootTaskId}>
      {branchIds.map((id) => (
        <div key={id} className="terra-task-branch-view__branch">{id}</div>
      ))}
    </div>
  )
}

export default TaskBranchView
