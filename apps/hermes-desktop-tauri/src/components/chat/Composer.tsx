interface ComposerProps {
  value?: string
  onChange?: (value: string) => void
  onSubmit?: () => void
  onStop?: () => void
  running?: boolean
}

export function Composer({ value = '', onChange, onSubmit, onStop, running }: ComposerProps) {
  return (
    <div className="terra-composer">
      <textarea value={value} onChange={(e) => onChange?.(e.target.value)} rows={3} />
      {running ? (
        <button type="button" onClick={onStop}>Stop</button>
      ) : (
        <button type="button" onClick={onSubmit}>Send</button>
      )}
    </div>
  )
}

export default Composer
