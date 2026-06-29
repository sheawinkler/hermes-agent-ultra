interface ReportArtifactPreviewProps {
  markdown: string
  onExport?: () => void
}

export function ReportArtifactPreview({ markdown, onExport }: ReportArtifactPreviewProps) {
  return (
    <article className="terra-report-artifact-preview">
      <pre>{markdown}</pre>
      <button type="button" onClick={onExport}>Export</button>
    </article>
  )
}

export default ReportArtifactPreview
