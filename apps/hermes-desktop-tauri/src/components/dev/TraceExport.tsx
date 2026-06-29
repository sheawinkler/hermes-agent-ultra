interface TraceExportProps {
  onExport?: (format: 'otel' | 'langfuse') => void
}

export function TraceExport({ onExport }: TraceExportProps) {
  return (
    <div className="terra-trace-export">
      <button type="button" onClick={() => onExport?.('otel')}>Export OTEL</button>
      <button type="button" onClick={() => onExport?.('langfuse')}>Export Langfuse</button>
    </div>
  )
}

export default TraceExport
