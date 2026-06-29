interface ComposerAttachmentsProps {
  files?: File[]
  onRemove?: (index: number) => void
}

export function ComposerAttachments({ files = [], onRemove }: ComposerAttachmentsProps) {
  return (
    <ul className="terra-composer-attachments">
      {files.map((file, i) => (
        <li key={`${file.name}-${i}`}>
          {file.name}
          <button type="button" onClick={() => onRemove?.(i)}>Remove</button>
        </li>
      ))}
    </ul>
  )
}

export default ComposerAttachments
