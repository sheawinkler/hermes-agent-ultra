interface PromptEditorProps {
  value?: string
  onChange?: (value: string) => void
  onRun?: () => void
}

export function PromptEditor({ value = '', onChange, onRun }: PromptEditorProps) {
  return (
    <div className="terra-prompt-editor">
      <textarea value={value} onChange={(e) => onChange?.(e.target.value)} rows={8} />
      <button type="button" onClick={onRun}>Run</button>
    </div>
  )
}

export default PromptEditor
