import { useStore } from '@nanostores/react'
import { useCallback, useState } from 'react'
import { Link, Route, Routes } from 'react-router-dom'

import TerraSettings from '@/app/terra/settings'

import { ApprovalModal } from '@/components/approval/ApprovalModal'
import { VerticalPicker } from '@/components/home/VerticalPicker'
import { VerticalSearch } from '@/components/home/VerticalSearch'
import { TaskDetail } from '@/components/tasks/TaskDetail'
import { TaskListRail } from '@/components/tasks/TaskListRail'
import { RightRailSwitcher, type RightRailMode } from '@/components/tasks/RightRailSwitcher'
import { TaskBranchView } from '@/components/tasks/TaskBranchView'
import { TaskMinimap } from '@/components/tasks/TaskMinimap'
import { TaskOutline } from '@/components/tasks/TaskOutline'
import { TaskProgressDashboard } from '@/components/tasks/TaskProgressDashboard'
import {
  useCancelTaskMutation,
  useContinueTaskMutation,
  useCreateTaskMutation,
  useTaskEventsQuery,
  useTaskQuery,
  useTasksQuery,
  useVerticalsQuery
} from '@/hooks/use-task-queries'
import {
  branchIdsFromEvents,
  minimapColorForKind,
  outlineItemsFromEvents,
  progressFromEvents
} from '@/lib/task-event-utils'
import { headApproval, resolveHeadApproval } from '@/stores/approval-queue'
import { $activeTaskId, $unreadTaskIds, setActiveTaskId, setSelectedVerticalId } from '@/stores/active-task'

export default function TerraApp() {
  const activeTaskId = useStore($activeTaskId)
  const unreadIds = useStore($unreadTaskIds)
  const pendingApproval = useStore(headApproval)

  const [verticalQuery, setVerticalQuery] = useState('')
  const [composerDraft, setComposerDraft] = useState('')
  const [rightRailMode, setRightRailMode] = useState<RightRailMode>('minimap')

  const tasksQuery = useTasksQuery()
  const taskQuery = useTaskQuery(activeTaskId)
  const eventsQuery = useTaskEventsQuery(activeTaskId)
  const verticalsQuery = useVerticalsQuery(verticalQuery)

  const createTaskMutation = useCreateTaskMutation()
  const continueTaskMutation = useContinueTaskMutation(activeTaskId)
  const cancelTaskMutation = useCancelTaskMutation(activeTaskId)

  const handleVerticalSelect = useCallback(
    async (verticalId: string) => {
      setSelectedVerticalId(verticalId)
      const result = await createTaskMutation.mutateAsync({
        title: `New ${verticalId} task`,
        vertical: verticalId,
        instruction: ''
      })
      setActiveTaskId(result.task.id)
      setComposerDraft('')
    },
    [createTaskMutation]
  )

  const handleComposerSubmit = useCallback(async () => {
    const instruction = composerDraft.trim()
    if (!instruction) return

    if (!activeTaskId) {
      const vertical = verticalsQuery.data?.verticals[0]?.id
      const result = await createTaskMutation.mutateAsync({
        title: instruction.slice(0, 80),
        vertical,
        instruction
      })
      setActiveTaskId(result.task.id)
      setComposerDraft('')
      return
    }

    await continueTaskMutation.mutateAsync(instruction)
    setComposerDraft('')
  }, [activeTaskId, composerDraft, continueTaskMutation, createTaskMutation, verticalsQuery.data?.verticals])

  const events = eventsQuery.data?.events ?? []
  const minimapAnchors = events.map(event => ({
    id: event.anchor_slug,
    color: minimapColorForKind(event.kind)
  }))
  const outlineItems = outlineItemsFromEvents(events)
  const branchIds = branchIdsFromEvents(events)
  const taskProgress = progressFromEvents(events)

  const jumpToAnchor = (anchorId: string) => {
    document.getElementById(anchorId)?.scrollIntoView({ behavior: 'smooth' })
  }

  const shell = (
    <div className="terra-shell">
      <header className="terra-shell__top">
        <Link to="/terra/settings">Settings</Link>
      </header>
      <TaskListRail
        tasks={tasksQuery.data?.tasks ?? []}
        selectedId={activeTaskId}
        unreadIds={unreadIds}
        loading={tasksQuery.isLoading}
        onSelect={setActiveTaskId}
      />

      <main className="terra-shell__main">
        {!activeTaskId ? (
          <section className="terra-home">
            <VerticalSearch query={verticalQuery} onQueryChange={setVerticalQuery} />
            <VerticalPicker
              verticals={verticalsQuery.data?.verticals ?? []}
              loading={verticalsQuery.isLoading}
              onSelect={verticalId => void handleVerticalSelect(verticalId)}
            />
          </section>
        ) : taskQuery.data ? (
          <TaskDetail
            task={taskQuery.data}
            events={eventsQuery.data?.events ?? []}
            eventsLoading={eventsQuery.isLoading}
            composerValue={composerDraft}
            onComposerChange={setComposerDraft}
            onComposerSubmit={() => void handleComposerSubmit()}
            onComposerStop={() => void cancelTaskMutation.mutate()}
            rightRail={
              <aside className="terra-right-rail">
                <TaskProgressDashboard progress={taskProgress} />
                <RightRailSwitcher
                  mode={rightRailMode}
                  onChange={setRightRailMode}
                  showBranch={branchIds.length > 0}
                />
                {rightRailMode === 'minimap' ? (
                  <TaskMinimap anchors={minimapAnchors} onJump={jumpToAnchor} />
                ) : null}
                {rightRailMode === 'outline' ? (
                  <TaskOutline items={outlineItems} onSelect={jumpToAnchor} />
                ) : null}
                {rightRailMode === 'branch' && activeTaskId ? (
                  <TaskBranchView rootTaskId={activeTaskId} branchIds={branchIds} />
                ) : null}
              </aside>
            }
          />
        ) : (
          <p className="terra-shell__loading">...</p>
        )}
      </main>

      <ApprovalModal
        open={Boolean(pendingApproval)}
        summary={pendingApproval?.summary ?? ''}
        onApprove={() => void resolveHeadApproval(true)}
        onReject={() => void resolveHeadApproval(false)}
        onClose={() => undefined}
      />
    </div>
  )

  return (
    <Routes>
      <Route
        path="settings"
        element={
          <div className="terra-shell">
            <nav className="terra-shell__nav">
              <Link to="/terra">← Tasks</Link>
            </nav>
            <TerraSettings />
          </div>
        }
      />
      <Route path="*" element={shell} />
    </Routes>
  )
}
