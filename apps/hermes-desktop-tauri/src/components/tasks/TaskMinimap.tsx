interface TaskMinimapProps {
  anchors?: { id: string; color: string }[]
  onJump?: (id: string) => void
}

export function TaskMinimap({ anchors = [], onJump }: TaskMinimapProps) {
  return (
    <div className="terra-task-minimap">
      {anchors.map((a) => (
        <button key={a.id} type="button" style={{ background: a.color }} onClick={() => onJump?.(a.id)} aria-label={a.id} />
      ))}
    </div>
  )
}

export default TaskMinimap
