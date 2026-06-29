import { useState } from 'react'

import { useT } from '@/i18n/useT'

interface ScheduleEditorProps {
  onSave?: (payload: { name: string; cron: string; vertical?: string }) => void
}

export function ScheduleEditor({ onSave }: ScheduleEditorProps) {
  const t = useT('schedules')
  const [name, setName] = useState('')
  const [cron, setCron] = useState('0 9 * * *')

  return (
    <form
      className="terra-schedule-editor"
      onSubmit={event => {
        event.preventDefault()
        onSave?.({ name, cron })
      }}
    >
      <h3>{t('editor.title', 'New schedule')}</h3>
      <label>
        {t('editor.name', 'Name')}
        <input value={name} onChange={e => setName(e.target.value)} required />
      </label>
      <label>
        {t('editor.cron', 'Cron expression')}
        <input value={cron} onChange={e => setCron(e.target.value)} required />
      </label>
      <p className="terra-schedule-editor__friendly">{t('editor.friendly', 'Daily at 09:00')}</p>
      <button type="submit">{t('editor.save', 'Save schedule')}</button>
    </form>
  )
}

export default ScheduleEditor
