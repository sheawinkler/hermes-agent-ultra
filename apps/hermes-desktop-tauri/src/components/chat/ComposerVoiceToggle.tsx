interface ComposerVoiceToggleProps {
  recording?: boolean
  onToggle?: () => void
}

export function ComposerVoiceToggle({ recording, onToggle }: ComposerVoiceToggleProps) {
  return (
    <button type="button" className="terra-composer-voice-toggle" aria-pressed={recording} onClick={onToggle}>
      {recording ? 'Stop' : 'Voice'}
    </button>
  )
}

export default ComposerVoiceToggle
