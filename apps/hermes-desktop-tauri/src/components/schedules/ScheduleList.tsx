import { useEffect, useState } from 'react'

import { useT } from '@/i18n/useT'

interface Schedule {
  id: string
  name: string
  cron: string
  next_run?: string
}

export function ScheduleList() {
  const t = useT('schedules')
  const [schedules, setSchedules] = useState<Schedule[]>([])

  useEffect(() => {
    void fetch('/api/schedules')
      .then(res => (res.ok ? res.json() : { schedules: [] }))
      .then(body => setSchedules((body as { schedules?: Schedule[] }).schedules ?? []))
      .catch(() => setSchedules([]))
  }, [])

  return (
    <section className="terra-schedule-list">
      <h3>{t('list.title', 'Scheduled tasks')}</h3>
      <ul>
        {schedules.length === 0 ? (
          <li>{t('list.empty', 'No schedules yet.')}</li>
        ) : (
          schedules.map(item => (
            <li key={item.id}>
              <strong>{item.name}</strong>
              <span>{item.cron}</span>
              {item.next_run ? <time>{item.next_run}</time> : null}
            </li>
          ))
        )}
      </ul>
    </section>
  )
}

export default ScheduleList
