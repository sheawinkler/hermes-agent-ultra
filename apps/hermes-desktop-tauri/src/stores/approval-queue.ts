import { atom } from 'nanostores'

export interface ApprovalQueueItem {
  taskId: string
  eventId: string
  summary: string
}

export const approvalQueue = atom<ApprovalQueueItem[]>([])

export function enqueueApproval(item: ApprovalQueueItem) {
  approvalQueue.set([...approvalQueue.get(), item])
}

export function dequeueApproval(eventId: string) {
  approvalQueue.set(approvalQueue.get().filter((i) => i.eventId !== eventId))
}
