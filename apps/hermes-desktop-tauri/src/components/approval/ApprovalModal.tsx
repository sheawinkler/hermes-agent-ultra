interface ApprovalModalProps {
  open: boolean
  summary: string
  onApprove?: () => void
  onReject?: () => void
  onClose?: () => void
}

export function ApprovalModal({ open, summary, onApprove, onReject, onClose }: ApprovalModalProps) {
  if (!open) return null
  return (
    <dialog open className="terra-approval-modal">
      <p>{summary}</p>
      <button type="button" onClick={onApprove}>Approve</button>
      <button type="button" onClick={onReject}>Reject</button>
      <button type="button" onClick={onClose}>Close</button>
    </dialog>
  )
}

export default ApprovalModal
