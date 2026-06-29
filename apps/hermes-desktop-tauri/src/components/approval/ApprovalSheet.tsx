interface ApprovalSheetProps {
  open: boolean
  summary: string
  onApprove?: () => void
  onReject?: () => void
  onClose?: () => void
}

export function ApprovalSheet({ open, summary, onApprove, onReject, onClose }: ApprovalSheetProps) {
  if (!open) return null
  return (
    <div className="terra-approval-sheet" role="dialog">
      <p>{summary}</p>
      <button type="button" onClick={onApprove}>Approve</button>
      <button type="button" onClick={onReject}>Reject</button>
      <button type="button" onClick={onClose}>Close</button>
    </div>
  )
}

export default ApprovalSheet
